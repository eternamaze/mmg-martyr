//! # Martyr - 主权内存模型
//!
//! 通过编译期生命周期禁锢实现零泄露资源管理。
//!
//! ## 安全保证
//!
//! - **引用不可逃逸**：HRTB (`for<'a>`) 确保闭包返回值不能携带资源引用
//! - **访客审计**：运行时计数 + 殉道检查确保无滞留访客
//! - **单点主权**：`Sovereign` 是唯一强所有者，`Lease` 仅为弱观察者

use parking_lot::RwLock;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Weak};

/// Discipline defines how to handle violations (e.g., accessing a killed resource).
pub trait Discipline: Send + Sync + 'static {
    fn punish(action: &'static str) -> !;
}

/// Default discipline: Panic.
pub struct PanicDiscipline;

impl Discipline for PanicDiscipline {
    fn punish(action: &'static str) -> ! {
        panic!("🔥 [Martyr] Sovereign violation! Action: {}", action);
    }
}

/// Internal status of a resource.
struct ResourceStatus {
    visitor_count: AtomicIsize,
    is_killed: AtomicBool,
}

/// The cell holding the resource and its status.
struct SovereignCell<T> {
    instance: T,
    status: ResourceStatus,
}

/// The Sovereign container. Manages the lifecycle of a single resource `T`.
/// It holds the strong ownership of the resource.
///
/// # 注意
///
/// 必须持有此值直到资源不再需要。丢弃 `Sovereign` 会触发殉道审计。
#[must_use = "Sovereign 被丢弃会立即触发资源销毁和殉道审计，请确保持有它直到资源不再需要"]
pub struct Sovereign<T, D: Discipline = PanicDiscipline> {
    // We use RwLock<Option<Arc>> to allow "taking" the resource out (killing it)
    // while the Sovereign struct itself remains valid (but empty).
    // This is crucial for explicit kill operations.
    inner: RwLock<Option<Arc<SovereignCell<T>>>>,
    _marker: PhantomData<D>,
}

impl<T, D: Discipline> Sovereign<T, D> {
    /// Create a new Sovereign container protecting the given resource.
    /// Returns the Sovereign (Owner) and a Lease (Weak Handle).
    pub fn new(resource: T) -> (Self, Lease<T, D>) {
        let cell = Arc::new(SovereignCell {
            instance: resource,
            status: ResourceStatus {
                visitor_count: AtomicIsize::new(0),
                is_killed: AtomicBool::new(false),
            },
        });

        let lease = Lease {
            cell: Arc::downgrade(&cell),
            _marker: PhantomData,
        };

        let sovereign = Self {
            inner: RwLock::new(Some(cell)),
            _marker: PhantomData,
        };

        (sovereign, lease)
    }

    /// Issue a new Lease to this Sovereign's resource.
    ///
    /// # Safety (Logical)
    ///
    /// This is safe because:
    /// - Lease only holds a Weak reference (cannot extend lifetime)
    /// - Lease cannot upgrade to strong reference (by design)
    /// - Multiple Leases coexist safely
    ///
    /// # Returns
    ///
    /// - `Some(Lease)` if the resource is still alive
    /// - `None` if the resource has been killed
    pub fn issue_lease(&self) -> Option<Lease<T, D>> {
        let lock = self.inner.read();
        lock.as_ref().map(|cell| Lease {
            cell: Arc::downgrade(cell),
            _marker: PhantomData,
        })
    }

    /// Kill the resource immediately.
    /// This will:
    /// 1. Mark the resource as killed (preventing new visitors).
    /// 2. Check for active visitors (panic if any).
    /// 3. Drop the strong reference to the resource (physically releasing it if no visitors).
    pub fn kill(&self) {
        let mut lock = self.inner.write();
        if let Some(cell) = lock.take() {
            // 1. Signal Kill
            cell.status.is_killed.store(true, Ordering::SeqCst);

            // 2. Check for lingering visitors
            let visitors = cell.status.visitor_count.load(Ordering::SeqCst);
            if visitors > 0 {
                panic!("💥 [Martyr] Force kill executed! {} visitors lingering. System self-destruct.", visitors);
            }

            // 3. Drop Arc (happens when `cell` goes out of scope here)
            tracing::debug!("✅ [Martyr] Resource killed cleanly.");
        }
    }
}

impl<T, D: Discipline> Drop for Sovereign<T, D> {
    fn drop(&mut self) {
        // Ensure we kill properly on drop
        self.kill();
    }
}

/// A Lease is a safe handle to a sovereign resource.
/// It does not own the resource, but allows controlled access.
pub struct Lease<T, D: Discipline = PanicDiscipline> {
    cell: Weak<SovereignCell<T>>,
    _marker: PhantomData<D>,
}

impl<T, D: Discipline> Clone for Lease<T, D> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T, D: Discipline> Lease<T, D> {
    /// 安全访问受保护资源。
    ///
    /// 闭包 `f` 在哨兵上下文中执行，资源引用 `&T` **不可能**逃逸出闭包。
    ///
    /// # 编译期安全保证
    ///
    /// 通过 `for<'a>` (HRTB) 约束，返回值 `R` 必须独立于资源的生命周期。
    /// 任何尝试返回资源引用的代码都会在**编译期**被拒绝：
    ///
    /// ```compile_fail
    /// # use mmg_martyr::{Sovereign, Lease};
    /// # let (sovereign, lease) = Sovereign::<String>::new("data".into());
    /// // 编译错误：返回值生命周期依赖闭包参数
    /// let escaped: &str = lease.access("steal", |s| s.as_str()).unwrap();
    /// ```
    pub fn access<F, R>(&self, action: &'static str, f: F) -> Result<R, AccessError>
    where
        F: for<'a> FnOnce(&'a T) -> R,
    {
        // 1. Upgrade Weak to Arc. If fails, resource is gone.
        let cell = self.cell.upgrade().ok_or(AccessError::ResourceNotFound)?;

        // 2. Check-in
        cell.status.visitor_count.fetch_add(1, Ordering::SeqCst);
        
        // RAII guard for Check-out
        let _guard = VisitorGuard {
            status: &cell.status,
        };

        // 3. Check if killed (Before execution)
        if cell.status.is_killed.load(Ordering::SeqCst) {
            D::punish(action);
        }

        // 4. Execute
        let result = f(&cell.instance);

        Ok(result)
    }
}

struct VisitorGuard<'a> {
    status: &'a ResourceStatus,
}

impl<'a> Drop for VisitorGuard<'a> {
    fn drop(&mut self) {
        self.status.visitor_count.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(thiserror::Error, Debug)]
pub enum AccessError {
    #[error("Resource not found or already killed")]
    ResourceNotFound,
}
