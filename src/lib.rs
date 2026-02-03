//! # Martyr - 殉道者
//!
//! 资源的唯一守护者。
//!
//! ## 殉道者的誓言
//!
//! > "我可以被无数人指向，但绝不泄露我誓死保卫的资源。"
//!
//! ## 核心原则
//!
//! - **唯一指针**：系统中只有 Martyr 持有指向资源 T 内存布局的指针
//! - **代理访问**：外部通过 `__invoke` 代理操作，永远无法获得指向 T 的指针
//! - **壳可共享**：Martyr 可以被 `Arc` 包裹共享，因为共享的只是"壳"
//! - **资源不泄露**：T 的内存布局物理上只有 Martyr 一个入口，kill 时必死无疑
//!
//! ## 双层防护
//!
//! ```text
//! 外层（Martyr 负责）：HRTB 约束，防止 &T 逃逸
//! 内层（NoLeakPledge 契约）：T 承诺不会通过方法返回指向自身内存布局的指针
//! ```
//!
//! ## 使用方式
//!
//! ```ignore
//! use mmg_martyr::{Martyr, NoLeakPledge};
//!
//! struct MyResource { /* ... */ }
//!
//! // 1. 声明遵守契约
//! impl NoLeakPledge for MyResource {}
//!
//! // 2. 为 Martyr<T> 实现 trait
//! impl MyTrait for Martyr<MyResource> {
//!     fn operation(&self) -> i32 {
//!         self.__invoke(|r| r.compute()).unwrap_or(0)
//!     }
//! }
//!
//! // 3. 使用
//! let martyr = Martyr::new(my_resource);
//! martyr.operation();
//! martyr.kill();
//! ```

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use parking_lot::RwLock;

// ============================================================================
// NoLeakPledge - 不泄露承诺
// ============================================================================

/// 不泄露承诺 — 宣誓类型不会泄露自身的内存布局
///
/// # ⚠️ 警告：这是人工契约，编译器无法验证
///
/// **实现此 trait 前，您必须诚实回答以下问题：**
///
/// 1. 类型 T 是否有方法返回指向 **T 自身内存布局** 的指针或引用？
/// 2. 这些指针/引用是否会逃逸到 `__invoke` 闭包外部？
///
/// **如果答案为"是"，则必须确保这些引用只在 `__invoke` 闭包内使用，
/// 最终返回值必须是 owned 值或 `'static` 生命周期。**
///
/// # 契约语义（精确定义）
///
/// 被 `Martyr<T>` 包装的资源 T，其**自身内存布局**（T 类型的结构体实例）
/// 必须只能通过 Martyr 访问。具体而言：
///
/// - **受保护的**：T 自身的内存布局（struct 的字段们占据的连续内存）
/// - **不受限制的**：T 内部字段所指向的其他内存（如 T 持有的 Arc 指向的资源）
///
/// ## 理解示例
///
/// ```text
/// struct Scheduler {
///     id: u64,                    // ← 这8字节属于 Scheduler 的内存布局
///     pool: Arc<ConnectionPool>,  // ← 这16字节(指针)属于 Scheduler 的内存布局
///                                 //   但 ConnectionPool 本身在另一段内存，不受保护
/// }
/// ```
///
/// Martyr 保护的是 Scheduler 的 24 字节，不是 ConnectionPool 的内存。
/// 所以 `pool.clone()` 返回 Arc 是合法的——它指向第三方内存。
///
/// # 为什么不是 unsafe trait？
///
/// 这是一个**君子协定**。编译器无法验证这些规则，实现者必须人工保证。
/// 违反契约不会导致内存安全问题（UB），只会导致 Martyr 的生命周期保护失效
/// ——这是逻辑错误，不是内存错误。
///
/// # 合规示例
///
/// ```
/// use mmg_martyr::NoLeakPledge;
///
/// // ✅ 纯值类型
/// struct Counter { value: i32 }
/// impl NoLeakPledge for Counter {}
///
/// // ✅ 原子类型
/// struct AtomicState { flag: std::sync::atomic::AtomicBool }
/// impl NoLeakPledge for AtomicState {}
///
/// // ✅ ZST（零大小类型）
/// struct EmptyMarker;
/// impl NoLeakPledge for EmptyMarker {}
/// ```
pub trait NoLeakPledge: Sized {}

// ============================================================================
// Martyr - 殉道者
// ============================================================================

/// 殉道者 — 资源的唯一守护者
///
/// # 内存布局
///
/// ```text
/// Martyr<T>
/// ├── inner: RwLock<Option<T>>  ← T 被 RwLock 保护
/// ├── is_killed: AtomicBool     ← 死亡标记
/// └── visitor_count: AtomicIsize ← 访客计数
/// ```
pub struct Martyr<T> {
    /// 被保护的资源 — 通过 RwLock 保护，无需 unsafe
    inner: RwLock<Option<T>>,
    /// 死亡标记
    is_killed: AtomicBool,
    /// 访客计数（调试用）
    visitor_count: AtomicIsize,
}

impl<T: NoLeakPledge> Martyr<T> {
    /// 创建殉道者，托管资源
    ///
    /// 从此刻起，T 的内存布局只存在于 Martyr 内部。
    pub fn new(resource: T) -> Self {
        Self {
            inner: RwLock::new(Some(resource)),
            is_killed: AtomicBool::new(false),
            visitor_count: AtomicIsize::new(0),
        }
    }

    /// 杀死资源（非协商式）
    ///
    /// # Panics
    ///
    /// 当有访客正在访问时，触发殉葬（panic）。
    pub fn kill(&self) {
        // 获取写锁
        let mut guard = self.inner.write();

        // 标记死亡
        self.is_killed.store(true, Ordering::SeqCst);

        // 检查访客
        let visitors = self.visitor_count.load(Ordering::SeqCst);
        if visitors > 0 {
            panic!(
                "💀 [Martyr] {} visitors still accessing! Martyrdom triggered.",
                visitors
            );
        }

        // 销毁资源
        if guard.take().is_some() {
            tracing::debug!("✅ [Martyr] Resource killed cleanly.");
        }
    }

    /// 资源是否还活着
    #[inline]
    pub fn is_alive(&self) -> bool {
        !self.is_killed.load(Ordering::SeqCst)
    }

    /// 代理调用 — **仅限 impl Trait for Martyr<T> 使用**
    ///
    /// # ⚠️ 警告：危险的内部 API
    ///
    /// 双下划线前缀表示这是一个**需要理解契约才能使用**的方法。
    ///
    /// # HRTB 约束
    ///
    /// `for<'a> FnOnce(&'a T) -> R` 确保返回值 `R` 不依赖 `&T` 的生命周期。
    /// 这从编译层面阻止了 `&T` 或其内部引用逃逸到闭包外部。
    ///
    /// # 正确用法
    ///
    /// ```ignore
    /// // ✅ 返回值类型（Copy 或 owned）
    /// self.__invoke(|r| r.get_count())
    ///
    /// // ✅ 内部引用在闭包内消费，返回 owned 值
    /// self.__invoke(|r| r.endpoint().to_string())
    ///
    /// // ✅ 返回 T 持有的外部 Arc 克隆（指向第三方内存）
    /// self.__invoke(|r| r.connection_pool.clone())
    ///
    /// // ✅ 返回 'static Future（必须完全自包含）
    /// self.__invoke(|r| r.create_request())  // 返回 BoxFuture<'static, ...>
    /// ```
    #[doc(hidden)]
    pub fn __invoke<F, R>(&self, f: F) -> Result<R, MartyrError>
    where
        F: for<'a> FnOnce(&'a T) -> R,
    {
        // 检查是否已死
        if self.is_killed.load(Ordering::SeqCst) {
            return Err(MartyrError::ResourceKilled);
        }

        // 获取读锁
        let guard = self.inner.read();

        // 访客登记
        self.visitor_count.fetch_add(1, Ordering::SeqCst);
        let _visitor = VisitorGuard {
            count: &self.visitor_count,
        };

        // 执行操作
        let resource = guard.as_ref().ok_or(MartyrError::ResourceKilled)?;
        Ok(f(resource))
    }

    /// 可变代理调用 — **仅限 impl Trait for Martyr<T> 使用**
    ///
    /// 参见 `__invoke` 的文档说明。
    #[doc(hidden)]
    pub fn __invoke_mut<F, R>(&self, f: F) -> Result<R, MartyrError>
    where
        F: for<'a> FnOnce(&'a mut T) -> R,
    {
        // 检查是否已死
        if self.is_killed.load(Ordering::SeqCst) {
            return Err(MartyrError::ResourceKilled);
        }

        // 获取写锁
        let mut guard = self.inner.write();

        // 访客登记
        self.visitor_count.fetch_add(1, Ordering::SeqCst);
        let _visitor = VisitorGuard {
            count: &self.visitor_count,
        };

        // 执行操作
        let resource = guard.as_mut().ok_or(MartyrError::ResourceKilled)?;
        Ok(f(resource))
    }
}

impl<T> Drop for Martyr<T> {
    fn drop(&mut self) {
        if !self.is_killed.load(Ordering::SeqCst) {
            self.is_killed.store(true, Ordering::SeqCst);
            let visitors = self.visitor_count.load(Ordering::SeqCst);
            if visitors > 0 {
                panic!(
                    "💀 [Martyr] Dropped with {} visitors! Martyrdom triggered.",
                    visitors
                );
            }
        }
    }
}

// ============================================================================
// VisitorGuard - RAII 访客守卫
// ============================================================================

struct VisitorGuard<'a> {
    count: &'a AtomicIsize,
}

impl Drop for VisitorGuard<'_> {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::SeqCst);
    }
}

// ============================================================================
// MartyrError - 错误类型
// ============================================================================

/// Martyr 错误
#[derive(Debug, thiserror::Error)]
pub enum MartyrError {
    /// 资源已被杀死
    #[error("resource has been killed")]
    ResourceKilled,
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Counter {
        value: i32,
    }

    impl NoLeakPledge for Counter {}

    impl Counter {
        fn new(value: i32) -> Self {
            Self { value }
        }

        fn get(&self) -> i32 {
            self.value
        }

        fn increment(&mut self) {
            self.value += 1;
        }
    }

    trait CounterOps {
        fn get_value(&self) -> i32;
        fn inc(&self);
    }

    impl CounterOps for Martyr<Counter> {
        fn get_value(&self) -> i32 {
            self.__invoke(|c| c.get()).unwrap_or(0)
        }

        fn inc(&self) {
            let _ = self.__invoke_mut(|c| c.increment());
        }
    }

    #[test]
    fn test_basic_proxy() {
        let martyr = Martyr::new(Counter::new(42));
        assert_eq!(martyr.get_value(), 42);
        martyr.inc();
        assert_eq!(martyr.get_value(), 43);
    }

    #[test]
    fn test_kill() {
        let martyr = Martyr::new(Counter::new(42));
        assert!(martyr.is_alive());
        martyr.kill();
        assert!(!martyr.is_alive());
        assert_eq!(martyr.get_value(), 0);
    }

    #[test]
    fn test_arc_sharing() {
        let martyr = Arc::new(Martyr::new(Counter::new(42)));
        let martyr2 = Arc::clone(&martyr);

        assert_eq!(martyr.get_value(), 42);
        assert_eq!(martyr2.get_value(), 42);

        martyr2.kill();

        assert!(!martyr.is_alive());
        assert!(!martyr2.is_alive());
    }

    #[test]
    fn test_concurrent_access() {
        use std::thread;

        let martyr = Arc::new(Martyr::new(Counter::new(0)));
        let mut handles = vec![];

        for _ in 0..10 {
            let m = Arc::clone(&martyr);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    m.inc();
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(martyr.get_value(), 1000);
    }
}
