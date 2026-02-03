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
//! - **唯一指针**：系统中只有 Martyr 持有指向资源 T 的指针
//! - **代理访问**：外部通过为 `Martyr<T>` 实现的 trait 代理操作，永远无法获得 `&T`
//! - **壳可共享**：Martyr 可以被 `Arc` 包裹共享，因为共享的只是"壳"
//! - **资源不泄露**：T 的指针物理上只存在一份，kill 时必死无疑
//!
//! ## 双层防护
//!
//! ```text
//! 外层（Martyr 负责）：HRTB 约束，防止 &T 逃逸
//! 内层（Sealed 契约）：T 承诺不持有可泄露的共享指针
//! ```
//!
//! ## 使用方式
//!
//! ```ignore
//! use mmg_martyr::{Martyr, Sealed};
//!
//! struct MyResource { /* ... */ }
//!
//! // 1. 声明遵守契约
//! impl Sealed for MyResource {}
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
// Sealed - 不泄露契约
// ============================================================================

/// 不泄露契约 — 承诺类型不会泄露自身内部的任何指针
///
/// # 契约内容
///
/// 实现此 trait 的类型必须遵守以下规则：
///
/// 1. **无共享指针**：不持有 `Arc`、`Rc` 或任何可克隆的共享引用
/// 2. **无内部泄露**：所有方法的返回值要么是值类型，要么生命周期绑定到 `&self`
/// 3. **无裸指针暴露**：不提供获取内部裸指针的方法
///
/// # 为什么不是 unsafe trait？
///
/// 这是一个**君子协定**。编译器无法验证这些规则，实现者必须人工保证。
/// 我们选择不使用 `unsafe` 是因为：违反契约不会导致内存安全问题（UB），
/// 只会导致生命周期保护失效——这是逻辑错误，不是内存错误。
///
/// # 示例
///
/// ```
/// use mmg_martyr::Sealed;
///
/// struct SafeResource {
///     data: Vec<u8>,      // ✅ 值语义
///     count: i32,         // ✅ 值类型
/// }
///
/// // SafeResource 不持有共享指针，不泄露内部引用
/// impl Sealed for SafeResource {}
/// ```
pub trait Sealed: Sized {}

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

impl<T: Sealed> Martyr<T> {
    /// 创建殉道者，托管资源
    ///
    /// 从此刻起，T 的指针只存在于 Martyr 内部。
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
    /// # 为什么需要 HRTB
    ///
    /// `for<'a> FnOnce(&'a T) -> R` 确保返回值 `R` 不依赖 `&T` 的生命周期。
    /// 这从编译层面阻止了 `&T` 逃逸到闭包外部。
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

    // Counter 是纯值类型，遵守契约
    impl Sealed for Counter {}

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
