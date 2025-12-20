# Martyr (殉道者)

**主权临界区与生命周期禁锢协议 (Sovereign Critical Section & Lifetime Confinement Protocol)**

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

> “只要你还在触碰资源，我就能找到你并终结你；只要你离开了，你就再也无法触碰它。”

## 核心理念 / Philosophy

Martyr 是一个为高可靠性系统（如金融交易核心）设计的 Rust 库，旨在终结“所有权模糊”的时代。它通过强制性的**资源禁锢 (Resource Confinement)** 和 **双向哨兵 (Bi-directional Sentry)** 机制，确保资源生命周期的绝对确定性。

在 Martyr 的世界里：
*   **资源不属于使用者**：资源被“禁锢”在主权临界区内。
*   **访问即签署生死状**：进入临界区意味着接受主权中心的审计与潜在的“处决”。
*   **生命周期零泄露**：利用 Rust 的高阶闭包 (HRTB) 确保引用无法逃逸。

## 核心特性 / Features

*   **🔒 资源禁锢 (Resource Confinement)**
    利用 `for<'b> F: FnOnce(&'b T) -> R` 匿名生命周期，物理上杜绝资源引用逃逸出闭包的可能性。

*   **👮 双向哨兵 (Bi-directional Sentry)**
    全生命周期的实名审计。
    *   **Check-in**: 记录访问者，签署“处决契约”。
    *   **Check-out**: 基于 RAII 自动注销，证明“清白”。

*   **⚡ 主权处决 (Sovereign Execution)**
    当主权中心发起 `force_kill` 时：
    *   **瞬时熔断**：拒绝一切新的访问。
    *   **存量审计**：检测是否有滞留者。
    *   **同步殉葬**：若发现滞留者，直接触发 Panic (或自定义纪律)，宁可崩溃也不允许未定义的资源状态存在。

## 快速开始 / Quick Start

```rust
use mmg_martyr::{SovereignRegistry, PanicDiscipline};

struct DatabaseConnection {
    id: u32,
}

fn main() {
    // 1. 建立主权中心
    let registry = SovereignRegistry::<DatabaseConnection, PanicDiscipline>::new();

    // 2. 注册资源，移交所有权，换取唯一的 Key
    let db_conn = DatabaseConnection { id: 1001 };
    let key = registry.register(db_conn);

    // 3. 安全访问
    registry.access(key, |sentry| {
        // 此时你持有哨兵，但还未接触到资源
        
        // 4. 申请进入临界区
        sentry.execute("query_user", |conn| {
            println!("正在使用数据库连接: {}", conn.id);
            // 引用 conn 被禁锢在此闭包内，无法被带出
        });
    });

    // 5. 主权处决
    // 强制销毁资源。如果此时仍有线程滞留在 execute 闭包内，系统将 Panic。
    registry.force_kill(key);
}
```

## 架构设计 / Architecture

### 1. 权力根基：资源禁锢
开发者永远无法接触资源的真实句柄，只能接触到被哨兵严格监控的“镜像借用”。离开哨兵大门的那一刻，访客在物理上**不可能**持有任何关于资源的残留信息。

### 2. 最终审判：清场与殉葬
当 `force_kill` 发生时：
1.  **关灯**：原子开关翻转。
2.  **清点**：检查 `visitor_count`。
3.  **处决**：
    *   **名单为空** -> 安全物理析构。
    *   **名单不为空** -> 触发 `Discipline::punish` (默认 Panic)。

## 许可证 / License

本项目采用 [MIT License](LICENSE) 开源。

---
*Built for those who demand absolute certainty.*
