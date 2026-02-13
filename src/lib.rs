//! # Martyr — 唯一指针守卫
//!
//! 基于图论的资源保护：资源 T 在内存中只有一条入边（一个指针），
//! 由 Martyr 独占持有。外部通过 HRTB 约束的闭包代理访问，
//! 永远无法获得指向 T 的直接指针。
//!
//! ## 图论模型
//!
//! ```text
//! Arc ──→ Arc ──→ Martyr ──唯一边──→ T
//! Arc ──↗          ↑
//!              (多条入边指向壳)
//! ```
//!
//! - Martyr 到 T 的边是**唯一的**（`RwLock<Option<T>>`）
//! - Martyr 自身可被 `Arc` 共享（壳有多条入边）
//! - `kill()` = 切断唯一边 → T 不可达 → T 被销毁
//! - `invoke()` = 通过唯一边代理访问（HRTB 防止引用逃逸）
//!
//! 只要没有第二个直接指针，图论保证资源不泄露。

use parking_lot::RwLock;

/// 唯一边已被切断，资源不可达。
#[derive(Debug, PartialEq, thiserror::Error)]
#[error("resource killed")]
pub struct ResourceKilled;

/// 唯一指针守卫。
///
/// `Martyr(T) = RwLock(Option(T))`
///
/// - `Some(T)` = 唯一边存在，资源可达
/// - `None` = 唯一边已切断，资源已销毁
/// - `RwLock` = 并发安全的读写互斥
/// - HRTB 约束 = 闭包无法将 `&T` 逃逸到外部
pub struct Martyr<T> {
    resource: RwLock<Option<T>>,
}

impl<T> Martyr<T> {
    /// 建立从 Martyr 到 T 的唯一边。
    pub fn new(resource: T) -> Self {
        Self {
            resource: RwLock::new(Some(resource)),
        }
    }

    /// 切断唯一边。T 不可达，立即销毁。
    ///
    /// 返回 `true` 表示本次切断成功，`false` 表示边已不存在。
    pub fn kill(&self) -> bool {
        self.resource.write().take().is_some()
    }

    /// 唯一边是否仍然存在。
    pub fn is_alive(&self) -> bool {
        self.resource.read().is_some()
    }

    /// 通过唯一边代理读访问。
    ///
    /// HRTB 约束 `for<'a> FnOnce(&'a T) -> R` 确保 `&T` 无法逃逸。
    pub fn invoke<F, R>(&self, f: F) -> Result<R, ResourceKilled>
    where
        F: for<'a> FnOnce(&'a T) -> R,
    {
        let guard = self.resource.read();
        match guard.as_ref() {
            Some(resource) => Ok(f(resource)),
            None => Err(ResourceKilled),
        }
    }

    /// 通过唯一边代理写访问。
    ///
    /// HRTB 约束 `for<'a> FnOnce(&'a mut T) -> R` 确保 `&mut T` 无法逃逸。
    pub fn invoke_mut<F, R>(&self, f: F) -> Result<R, ResourceKilled>
    where
        F: for<'a> FnOnce(&'a mut T) -> R,
    {
        let mut guard = self.resource.write();
        match guard.as_mut() {
            Some(resource) => Ok(f(resource)),
            None => Err(ResourceKilled),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Counter(i32);

    impl Counter {
        fn get(&self) -> i32 {
            self.0
        }
        fn increment(&mut self) {
            self.0 += 1;
        }
    }

    #[test]
    fn invoke_delegates_to_resource() {
        let m = Martyr::new(Counter(42));
        assert_eq!(m.invoke(|c| c.get()), Ok(42));
    }

    #[test]
    fn invoke_mut_modifies_resource() {
        let m = Martyr::new(Counter(0));
        m.invoke_mut(|c| c.increment()).unwrap();
        assert_eq!(m.invoke(|c| c.get()), Ok(1));
    }

    #[test]
    fn kill_severs_the_edge() {
        let m = Martyr::new(Counter(42));
        assert!(m.is_alive());
        assert!(m.kill());
        assert!(!m.is_alive());
        assert!(m.invoke(|c| c.get()).is_err());
        // Second kill returns false (already dead)
        assert!(!m.kill());
    }

    #[test]
    fn arc_sharing_single_edge() {
        let m = Arc::new(Martyr::new(Counter(42)));
        let m2 = Arc::clone(&m);
        // Multiple paths to shell, but only one edge to resource
        assert_eq!(m.invoke(|c| c.get()), Ok(42));
        assert_eq!(m2.invoke(|c| c.get()), Ok(42));
        // Any holder can sever the edge
        m.kill();
        assert!(!m2.is_alive());
        assert!(m2.invoke(|c| c.get()).is_err());
    }

    #[test]
    fn concurrent_access() {
        let m = Arc::new(Martyr::new(Counter(0)));
        let mut handles = vec![];
        for _ in 0..10 {
            let m = Arc::clone(&m);
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    let _ = m.invoke_mut(|c| c.increment());
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(m.invoke(|c| c.get()), Ok(1000));
    }
}
