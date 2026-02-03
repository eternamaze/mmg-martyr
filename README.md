# Martyr - 殉道者

> **殉道者的誓言**：我可以被无数人指向，但绝不泄露我誓死保卫的资源。

## 哲学

传统智能指针（如 `Arc<T>`）的问题：**指针可以无限复制**。任何持有者都可以"拒绝释放"，生命周期由所有持有者的联合意志决定。

Martyr 的哲学：

| 概念 | Arc | Martyr |
|------|-----|--------|
| 资源指针 | 多份（每个 Arc 一份） | **唯一一份**（只在 Martyr 内部） |
| 生命周期控制 | 民主制（最后一个放手才释放） | 独裁制（kill 时必死） |
| 共享的是什么 | 资源本身的指针 | **容器的壳**（Arc<Martyr<T>>） |

```
Arc<T>（民主制）：          Martyr<T>（独裁制）：

  A ─→ Arc ──┐                A ─→ Arc ──┐
             ├──→ T                       ├──→ Martyr ──→ T
  B ─→ Arc ──┘                B ─→ Arc ──┘
                                          ↑
  任何持有者都能                    唯一指向 T 的指针
  阻止 T 的释放                    kill 时 T 必死
```

## 双层防护

**外层（Martyr 负责）**：
- HRTB 约束（`for<'a> FnOnce(&'a T) -> R`）确保 `&T` 无法逃逸
- 代理模式：外部只能通过 trait 方法操作，永远不直接获得 `&T`

**内层（Sealed 契约）**：
- T 必须实现 `Sealed` trait，承诺不持有可泄露的共享指针
- 这是君子协定——编译器无法验证，实现者人工保证

为什么需要两层？因为 HRTB 只能阻止 `&T` 直接逃逸。如果 T 内部持有 `Arc<Something>`，闭包仍可以 `Arc::clone()` 逃逸。

## 使用

```rust
use mmg_martyr::{Martyr, Sealed, MartyrError};

// 1. 定义资源
struct Database {
    data: Vec<String>,
}

// 2. 声明遵守契约（无共享指针、无内部泄露）
impl Sealed for Database {}

// 3. 定义接口 trait
trait DatabaseOps {
    fn query(&self, id: usize) -> Option<String>;
    fn insert(&self, value: String);
}

// 4. 为 Martyr<T> 实现 trait（不是为 T）
impl DatabaseOps for Martyr<Database> {
    fn query(&self, id: usize) -> Option<String> {
        self.__invoke(|db| db.data.get(id).cloned())
            .ok()
            .flatten()
    }
    
    fn insert(&self, value: String) {
        let _ = self.__invoke_mut(|db| db.data.push(value));
    }
}

// 5. 使用
fn main() {
    let db = Martyr::new(Database { data: vec![] });
    
    db.insert("hello".to_string());
    assert_eq!(db.query(0), Some("hello".to_string()));
    
    db.kill();  // 资源立即销毁
    assert_eq!(db.query(0), None);  // 已死
}
```

### Arc 共享

```rust
use std::sync::Arc;

let shared = Arc::new(Martyr::new(Database { data: vec![] }));
let holder1 = Arc::clone(&shared);
let holder2 = Arc::clone(&shared);

// 共享的是 Martyr（壳），不是 Database（资源）
// Database 的指针始终只有一份
```

## API

```rust
// 契约标记
pub trait Sealed: Sized {}

// 殉道者
pub struct Martyr<T> { /* ... */ }

impl<T: Sealed> Martyr<T> {
    pub fn new(resource: T) -> Self;
    pub fn is_alive(&self) -> bool;
    pub fn kill(&self);  // panic if visitors present
    
    // 仅供 trait 实现使用
    #[doc(hidden)]
    pub fn __invoke<F, R>(&self, f: F) -> Result<R, MartyrError>
    where F: for<'a> FnOnce(&'a T) -> R;
    
    #[doc(hidden)]
    pub fn __invoke_mut<F, R>(&self, f: F) -> Result<R, MartyrError>
    where F: for<'a> FnOnce(&'a mut T) -> R;
}

pub enum MartyrError {
    ResourceKilled,
}
```

## Sealed 契约

实现 `Sealed` 意味着承诺：

1. **无共享指针**：不持有 `Arc`、`Rc`
2. **无内部泄露**：方法返回值是值类型，或生命周期绑定到 `&self`
3. **无裸指针暴露**：不提供获取内部裸指针的方法

```rust
// ✅ 正确
struct Safe { data: Vec<u8>, count: i32 }
impl Sealed for Safe {}

// ❌ 违反契约
struct Leaky { shared: Arc<Data> }
impl Sealed for Leaky {}  // 危险！Arc 可被克隆逃逸
```

## 威胁模型

| 威胁 | 状态 | 机制 |
|------|------|------|
| `&T` 直接逃逸 | ✅ 阻止 | HRTB 约束 |
| T 内部 Arc 逃逸 | ⚠️ 契约 | Sealed 君子协定 |
| 访问已销毁资源 | ✅ 阻止 | MartyrError |
| 并发竞争 | ✅ 阻止 | RwLock |
| 硬件指针复制 | ❌ 不可能 | 冯·诺依曼架构限制 |

## 为什么叫"殉道者"？

殉道者为信仰献身：
- 殉道者誓死保护其内部的"道"（资源）
- 殉道者可以被无数人"指向"（引用）
- 但殉道者绝不泄露其保护的"道"的位置
- 当殉道者决定"殉道"（kill）时，资源立即消亡——只要它不背叛

## 关于图灵完备与安全性

用户问：为什么图灵完备的系统不能保证"不泄露"？

图灵完备描述**计算能力**（能算出什么结果），不描述**约束能力**（能阻止什么行为）。

你想要的"引用图入边约束"是数学上良定义的静态性质，但图灵机是关于动态计算的。这是两个正交的维度。类型系统、借用检查器等都是在图灵完备之上**额外添加的约束层**。

硬件层面（x86/ARM）无法实现"只允许特定位置的指针解引用到特定内存"——CPU 解引用时只看指针的值（一个数字），不追踪这个数字是从哪里加载的。这是冯·诺依曼架构的本质限制。

因此，Martyr 是在软件层面能做到的"尽力而为"。

## License

MIT
