//! # Martyr — 唯一边守卫
//!
//! 令 $G = (V, E)$ 为堆内存有向图，资源 T 占据子图 $S \subseteq G$。
//!
//! **不变量**：从 S 外部进入 S 的边恰好有一条 — Martyr 内的 `*mut T`。
//!
//! ```text
//! Martyr ──*mut T──→ [ T ──→ T.field ──→ ... ]
//!          ↑          └──────── S ────────────┘
//!       唯一边
//! ```
//!
//! 切断唯一边 → S 成为孤立子图 → `Box::from_raw` 回收。
//!
//! ## 安全性
//!
//! | 威胁 | 防御 |
//! |------|------|
//! | 引用逃逸 | HRTB `for<'a>` — 闭包无法捕获内部引用 |
//! | 并发竞争 | `RwLock` 读写互斥 |
//! | 悬垂指针 | `kill()` 先置空再释放 |
//! | 子图泄露 | T 的设计契约（计算理论边界） |

use parking_lot::RwLock;
use std::ptr;

/// 唯一边已被切断。资源子图不可达。
#[derive(Debug, PartialEq, thiserror::Error)]
#[error("resource killed")]
pub struct ResourceKilled;

/// 唯一边守卫。
///
/// `RwLock<*mut T>`：锁保护的就是唯一边本身。
/// 非空 = 边存在 = 资源可达。空 = 边已切断 = 资源已回收。
pub struct Martyr<T> {
    edge: RwLock<*mut T>,
}

// SAFETY: Martyr 独占堆上的 T。T: Send — kill() 可在任意线程回收。
unsafe impl<T: Send> Send for Martyr<T> {}
// SAFETY: invoke() 并发时多线程共享 &T。T: Sync — &T 跨线程安全。
unsafe impl<T: Send + Sync> Sync for Martyr<T> {}

impl<T> Martyr<T> {
    /// 将 T 移入堆，建立唯一边。
    ///
    /// `Box::into_raw(Box::new(resource))` 创建系统中唯一的 `*mut T`。
    pub fn new(resource: T) -> Self {
        Self {
            edge: RwLock::new(Box::into_raw(Box::new(resource))),
        }
    }

    /// 切断唯一边。T 的子图成为孤立分量，立即回收。
    ///
    /// 先置空再释放（若 `T::drop` 恐慌，边已断，不会双重释放）。
    pub fn kill(&self) -> bool {
        let mut edge = self.edge.write();
        let ptr = *edge;
        if ptr.is_null() {
            return false;
        }
        *edge = ptr::null_mut();
        // SAFETY: ptr 来自 Box::into_raw，非空。写锁保证独占。
        unsafe { drop(Box::from_raw(ptr)) };
        true
    }

    /// 边是否存在。
    pub fn is_alive(&self) -> bool {
        !self.edge.read().is_null()
    }

    /// 通过唯一边共享访问。读锁允许并发。
    ///
    /// HRTB `for<'a> FnOnce(&'a T) -> R` 确保 `&T` 无法逃逸。
    pub fn invoke<F, R>(&self, f: F) -> Result<R, ResourceKilled>
    where
        F: for<'a> FnOnce(&'a T) -> R,
    {
        let edge = self.edge.read();
        let ptr = *edge;
        if ptr.is_null() {
            return Err(ResourceKilled);
        }
        // SAFETY: ptr 非空，读锁阻止并发 kill/invoke_mut。
        Ok(f(unsafe { &*ptr }))
    }

    /// 通过唯一边独占访问。写锁保证互斥。
    ///
    /// HRTB `for<'a> FnOnce(&'a mut T) -> R` 确保 `&mut T` 无法逃逸。
    pub fn invoke_mut<F, R>(&self, f: F) -> Result<R, ResourceKilled>
    where
        F: for<'a> FnOnce(&'a mut T) -> R,
    {
        let edge = self.edge.write();
        let ptr = *edge;
        if ptr.is_null() {
            return Err(ResourceKilled);
        }
        // SAFETY: ptr 非空，写锁保证独占。
        Ok(f(unsafe { &mut *ptr }))
    }
}

impl<T> Drop for Martyr<T> {
    fn drop(&mut self) {
        // &mut self → 无其他引用 → get_mut 无需加锁
        let ptr = *self.edge.get_mut();
        if !ptr.is_null() {
            // SAFETY: ptr 来自 Box::into_raw，非空。
            unsafe { drop(Box::from_raw(ptr)) };
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
        assert!(!m.kill());
    }

    #[test]
    fn arc_sharing_single_edge() {
        let m = Arc::new(Martyr::new(Counter(42)));
        let m2 = Arc::clone(&m);
        assert_eq!(m.invoke(|c| c.get()), Ok(42));
        assert_eq!(m2.invoke(|c| c.get()), Ok(42));
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

    /// 验证 kill 确实回收资源（T::drop 被调用）。
    #[test]
    fn kill_reclaims_resource() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static DROPPED: AtomicBool = AtomicBool::new(false);
        struct Probe;
        impl Drop for Probe {
            fn drop(&mut self) {
                DROPPED.store(true, Ordering::Relaxed);
            }
        }
        DROPPED.store(false, Ordering::Relaxed);
        let m = Martyr::new(Probe);
        assert!(!DROPPED.load(Ordering::Relaxed));
        m.kill();
        assert!(DROPPED.load(Ordering::Relaxed));
    }

    /// 验证 kill 后 drop 不会双重释放。
    #[test]
    fn drop_after_kill_is_noop() {
        let m = Martyr::new(Counter(42));
        m.kill();
        drop(m); // 若双重释放则 UB/崩溃
    }
}
