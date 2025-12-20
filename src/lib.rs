#![allow(clippy::disallowed_types)]

use parking_lot::RwLock;
use slotmap::{new_key_type, SlotMap};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;

// 定义主权资源 Key，禁止手动伪造
new_key_type! { pub struct SovereignKey; }

/// 处决纪律 (Discipline)
/// 定义当发现违规入侵或滞留时，如何执行惩罚。
pub trait Discipline: Send + Sync + 'static {
    /// 处决逻辑
    /// action: 当前正在尝试执行的业务动作名称
    fn punish(action: &'static str) -> !;
}

/// 默认纪律：直接 Panic
pub struct PanicDiscipline;

impl Discipline for PanicDiscipline {
    fn punish(action: &'static str) -> ! {
        panic!("🔥 [Martyr] 区域已封锁，强行闯入者死！Action: {}", action);
    }
}

/// 主权中心状态：军事化监视
/// 包含访客计数和全局电闸
pub struct SovereigntyStatus {
    visitor_count: AtomicIsize, // 临界区内的活人计数
    is_killed: AtomicBool,      // 全局电闸
}

/// 访客令牌：RAII 强制打卡器
/// 离开作用域时自动注销访客计数
struct VisitorToken<'a> {
    status: &'a SovereigntyStatus,
}

impl<'a> Drop for VisitorToken<'a> {
    fn drop(&mut self) {
        // 离开临界区时物理注销 (Check-out)
        self.status.visitor_count.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 资源的“物理单间”
struct SovereignCell<T> {
    instance: T,
    // 每一个资源自带一个状态控制器，由主权中心共享控制
    status: Arc<SovereigntyStatus>,
}

/// 全局主权注册表 (Internal)
struct SovereignRegistryInternal<T> {
    // SlotMap 保证了物理所有权的唯一性和代际校验
    storage: RwLock<SlotMap<SovereignKey, SovereignCell<T>>>,
}

/// 哨兵句柄 (Sentry)
/// 双向哨兵：负责进入审计与离开注销
pub struct Sentry<'a, T, D: Discipline = PanicDiscipline> {
    inner: &'a T,
    status: &'a SovereigntyStatus,
    _marker: PhantomData<D>,
}

impl<'a, T, D: Discipline> Sentry<'a, T, D> {
    /// 【唯一的访问门户】
    /// execute 模式强制实现了“带不走”与“必须打卡”。
    ///
    /// - `action`: 业务动作名称，用于审计和处决日志。
    /// - `f`: 业务闭包。
    #[inline(always)]
    pub fn execute<F, R>(&self, action: &'static str, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        // 1. 进入登记 (Check-in)：建立处决连结
        self.status.visitor_count.fetch_add(1, Ordering::SeqCst);
        // RAII Token 确保离开时自动注销 (Check-out)
        let _token = VisitorToken { status: self.status };

        // 2. 主权检查：关灯后禁止进入
        if self.status.is_killed.load(Ordering::SeqCst) {
            D::punish(action);
        }

        // 3. 业务执行：资源在禁锢区内流动
        f(self.inner)
    }
}

/// 主权注册表句柄 (Safe Handle)
/// 这是一个引用计数句柄，指向底层的注册表。
/// 持有此句柄并不意味着持有资源的所有权，仅意味着有权访问注册表。
pub struct SovereignRegistry<T, D: Discipline = PanicDiscipline> {
    internal: Arc<SovereignRegistryInternal<T>>,
    _marker: PhantomData<D>,
}

impl<T, D: Discipline> Clone for SovereignRegistry<T, D> {
    fn clone(&self) -> Self {
        Self {
            internal: self.internal.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T, D: Discipline> Default for SovereignRegistry<T, D> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, D: Discipline> SovereignRegistry<T, D> {
    pub fn new() -> Self {
        Self {
            internal: Arc::new(SovereignRegistryInternal {
                storage: RwLock::new(SlotMap::with_key()),
            }),
            _marker: PhantomData,
        }
    }

    /// 【注册资源】
    /// 将资源移交给主权中心，返回一个 Key。
    pub fn register(&self, resource: T) -> SovereignKey {
        let mut map = self.internal.storage.write();
        map.insert(SovereignCell {
            instance: resource,
            status: Arc::new(SovereigntyStatus {
                visitor_count: AtomicIsize::new(0),
                is_killed: AtomicBool::new(false),
            }),
        })
    }

    /// 【主权指令：处决】
    /// 对应 OS 的 kill -9。不协商，不等待。
    /// 如果发现有线程滞留在临界区内，将触发同步 Panic。
    pub fn force_kill(&self, key: SovereignKey) {
        let mut map = self.internal.storage.write();

        if let Some(cell) = map.remove(key) {
            // 1. 瞬间关灯 (Signal Kill)
            cell.status.is_killed.store(true, Ordering::SeqCst);

            // 2. 终极审判：如果有人不离开，就让他们随系统一起崩溃
            let heavy_sleepers = cell.status.visitor_count.load(Ordering::SeqCst);
            if heavy_sleepers > 0 {
                panic!("💥 [主权处决] 发现 {} 名非法滞留者，执行系统自毁！", heavy_sleepers);
            }

            // 3. 物理销毁。资源在这一行被 Drop，Socket 关闭，内存释放。
            // 此时由于没有 Arc，没有任何人能阻止 T 的析构。
            let _ = cell.instance;

            tracing::info!("✅ [主权中心] 资源 ID: {:?} 已物理析构且逻辑断电。", key);
        }
    }

    /// 【受控进入】
    /// 只有通过这个入口，开发者才能触碰到 Sentry
    pub fn access<F, R>(&self, key: SovereignKey, f: F) -> R
    where
        F: for<'any> FnOnce(Sentry<'any, T, D>) -> R,
    {
        let map = self.internal.storage.read();
        let cell = map.get(key).expect("试图访问不存在的资源或资源已熔断");

        let sentry = Sentry {
            inner: &cell.instance,
            status: &cell.status,
            _marker: PhantomData,
        };

        // 运行开发者 A 的代码
        f(sentry)
    }
}
