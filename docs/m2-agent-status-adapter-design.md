# Kaku M2 设计稿：Agent 状态总线与多 Provider 适配

- 文档日期：2026-04-02
- 状态：In Progress（M2-1A/B 最小闭环已完成）
- 对应里程碑：M2（`docs/architecture-v1.md`）
- 关联需求：FR-3、FR-4（`docs/requirements-v1.md`）

## 1. 背景与目标

当前 Sidebar 的 `session.status` 主要来自 `sessions.json` 的字符串字段，已具备展示能力，但“状态来源”仍不统一，导致：
1. 运行时状态更新链路分散，后续扩展风险高。
2. 对 Codex / Claude 等不同 Agent 的状态识别缺少统一抽象。
3. `need_approve` 等关键状态缺少高置信度来源。

M2 的目标不是“绑定 Codex”，而是建立可插拔架构：
1. 核心层只认统一状态事件，不依赖具体 provider。
2. Provider 通过 Adapter 输出统一事件。
3. UI/持久化消费统一状态，支持降级和可观测性。

## 2. 范围与边界

本阶段（M2）包含：
1. 统一状态事件协议与状态机入口。
2. Adapter 接口与 Provider 能力抽象。
3. Codex 接入（优先 App Server；次选 `codex exec --json`）。
4. Claude 预留适配位（先可运行骨架 + fallback）。
5. Sidebar 消费统一状态并持久化。

本阶段不包含：
1. M3 的 Snippets/Env/Files 资源管理。
2. 背景图与自动可读性策略（FR-6/FR-7）。
3. 各 provider 的深度业务能力（如 diff 编辑器）。

## 3. 核心架构

### 3.1 分层

1. `Domain`: `SessionStatusManager`
- 唯一状态写入口。
- 执行状态迁移合法性校验。
- 统一发出 `session.status_changed`。

2. `Integration`: `AgentAdapter`
- 把 provider 原生事件翻译为统一 `AgentEvent`。
- 提供能力声明（是否支持审批、恢复、结构化事件）。

3. `UI/Storage`
- Sidebar 订阅统一状态并刷新。
- `sessions.json` 持久化最新状态与来源元信息。

### 3.2 统一事件协议（建议）

```rust
enum SessionStatus {
    Idle,
    Loading,
    Running,
    NeedApprove,
    Done,
    Error,
}

enum StatusSource {
    Structured, // provider 明确结构化事件
    Heuristic,  // 文本/上下文推断
}

enum StatusConfidence {
    High,
    Low,
}

enum AgentEvent {
    TaskStarted { provider: String },
    TaskOutput { provider: String },
    ApprovalRequired { provider: String, detail: Option<String> },
    ApprovalResolved { provider: String, approved: bool },
    TaskCompleted { provider: String },
    TaskFailed { provider: String, reason: Option<String> },
}
```

### 3.3 状态映射规则（统一）

1. `TaskStarted` -> `loading`（短暂）-> `running`
2. `TaskOutput` -> 保持 `running`
3. `ApprovalRequired` -> `need_approve`
4. `ApprovalResolved(approved=true)` -> `running`
5. `ApprovalResolved(approved=false)` -> `error`
6. `TaskCompleted` -> `done`
7. `TaskFailed` -> `error`

约束：
1. 非法跳转（如 `done -> loading`）默认拒绝，记录告警日志。
2. 同状态重复更新可合并（避免无意义刷盘）。

## 4. Provider 适配策略

### 4.1 Codex Adapter（首期主实现）

优先级：
1. 首选 `Codex App Server` 结构化事件（高置信度，支持审批状态）。
2. 备选 `codex exec --json` 事件流（高置信度，覆盖 running/done/error）。
3. 最后 fallback 到 shell 生命周期信号（中等置信度）。

说明：
1. 通过 Adapter 输出统一 `AgentEvent`，不将 `codex_*` 细节泄漏到 Domain/UI。
2. `need_approve` 仅在结构化审批事件存在时进入高置信度。

### 4.2 Claude Adapter（本阶段先落骨架）

策略：
1. 先提供 `ClaudeAdapter` 接口实现骨架与能力声明。
2. 若无结构化审批事件，先稳定支持 `running/done/error`。
3. `need_approve` 仅在明确信号可用时启用；文本匹配只作低置信度兜底。

## 5. 数据模型与存储变更

`sessions.json` 追加字段（向后兼容）：
1. `status_source`: `structured|heuristic`
2. `status_confidence`: `high|low`
3. `status_reason`: 最近一次状态变化原因（可选）
4. `agent_type`: `codex|claude|shell`（若缺省，按既有逻辑推断）

写入策略：
1. 沿用原子写入（tmp + rename）。
2. 高频更新做 debounce（例如 300ms）。
3. 退出前强制 flush 一次。

## 6. 代码落点（现有项目）

建议改造位置：
1. Sidebar 读写与渲染：`kaku-gui/src/termwindow/render/sidebar.rs`
2. 窗口事件总线入口：`kaku-gui/src/termwindow/mod.rs`
3. Lua 事件接收层（user-var、EmitEvent）：`assets/macos/Kaku.app/Contents/Resources/kaku.lua`

新增模块建议：
1. `kaku-gui/src/agent_status/manager.rs`
2. `kaku-gui/src/agent_status/events.rs`
3. `kaku-gui/src/agent_status/adapters/{mod.rs,codex.rs,claude.rs,shell_fallback.rs}`

## 7. 里程碑拆解（M2）

1. M2-1A：状态协议与管理器
- 定义统一事件与状态迁移规则。
- 接管状态写入口（不允许散落直接改 `status`）。
- 完成单元测试。

2. M2-1B：Adapter 架构
- 落地 `AgentAdapter` trait 与能力声明。
- 接入 `CodexAdapter`（至少覆盖 running/done/error）。
- `ClaudeAdapter` 骨架 + fallback 路径。

3. M2-2：Sidebar 消费总线
- Sidebar 改为消费运行时状态并同步持久化。
- 现有徽标映射保持兼容（I/L/?/R/D/E/U）。

4. M2-3：恢复语义闭环
- 重启恢复状态 + 来源置信度。
- 严格遵守“不自动续跑”。

## 8. 验收标准（M2）

1. 不同 provider 产生的状态事件可统一驱动 Sidebar。
2. `running/done/error` 在首期全链路稳定可用。
3. 支持审批信号的 provider 可准确进入 `need_approve`。
4. 重启后状态可恢复，且不会自动继续执行任务。
5. 状态更新异常时可降级并提示，不影响主流程可用性。

## 9. 风险与降级

1. Provider 事件不稳定：
- 降级到 shell 生命周期信号，仅保证 `running/done/error`。

2. `need_approve` 信号缺失：
- 保持 `running`，并标记 `status_confidence=low`，避免误导。

3. 高频刷盘导致性能抖动：
- debounce + 批量写入。

4. 旧数据兼容：
- 所有新增字段必须 `serde(default)`，避免读取旧文件失败。

## 10. 实施顺序建议

1. 先做 M2-1A（状态管理器 + 单元测试）。
2. 再做 M2-1B（CodexAdapter 最小闭环）。
3. 接着做 M2-2（Sidebar 消费总线）。
4. 最后做 M2-3（恢复语义与持久化收口）。

## 11. 当前落地进度（2026-04-02）

1. 已完成 M2-1A：
   - 统一事件协议、状态类型、状态机与单测已落地。
2. 已完成 M2-1B 最小可用：
   - 已有 `AgentAdapterRegistry` + `CodexAdapter` + `ClaudeAdapter` 骨架。
   - user-var 信号可驱动 sidebar session 状态更新并持久化。
3. 当前 Codex 接入形态：
   - 基于 shell user-var 的 heuristic 识别（`kaku_last_cmd` / `kaku_last_exit_code`）。
   - 结构化事件源（App Server / JSON stream）尚未接入。
4. 未完成项（后续）：
   - M2-2 进一步完善 UI 消费细节与更新节流策略。
   - M2-3 重启恢复语义闭环与“不自动续跑”验证。
