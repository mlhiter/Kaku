# Kaku 二开需求规格（V1 草案）

- 文档日期：2026-04-01
- 状态：Draft（基于聊天确认，待进入实现）
- 目标版本：V1（可交付）+ V1.5（后续增强）

## 1. 已冻结决策（本轮确认）

1. `可编辑 Diff` 放入 `V1.5`，不进入 V1。
2. 会话恢复语义：`只恢复界面和上下文，不自动续跑任务`。
3. 环境变量存储：V1 先使用 `明文存储`，不接 Keychain，不加密（风险已知且接受）。
4. 背景图能力：支持 `跟随图片变化` 自动调整终端文字样式，至少包含：
- 黑/白前景色自动切换
- 字重自动调整（加粗策略）

## 2. V1 目标

1. 建立项目化的 AI Terminal 工作流：一个项目内并行多个会话。
2. 让 AI 任务状态可视化：运行中、待审批、完成、失败可快速识别。
3. 支持会话恢复：重启后快速回到之前工作现场（不自动执行）。
4. 支持背景图定制，并自动保证文本可读性。

## 3. V1 范围（In Scope）

1. 左侧 Sidebar 信息架构：`Project -> Sessions -> Resources`。
2. Session `Pin` 能力（防误关）。
3. Session 状态徽标与颜色：`loading / need_approve / done / error`。
4. 会话持久化与恢复（布局、日志索引、AI 会话元数据）。
5. Sidebar 资源区：
- Snippets（常用命令片段）
- Env（环境变量，明文）
- Files（项目内轻量工作文件）
6. 背景图自定义：
- 选择本地图片
- 自动前景色黑白切换
- 自动字重切换

## 4. V1.5 范围（Out of Scope for V1）

1. Diff 界面内直接编辑并立即应用。
2. GitLens Lite 深化能力（变更树、hunk 批量、blame 深度联动）。
3. 环境变量加密存储与系统密钥链集成。
4. 更复杂的视觉自适应（区域级采样、多层渐变策略）。

## 5. 信息架构与导航

1. Sidebar 顶层固定区块：
- Projects
- Resources
- Global Actions（新建项目、新建会话、导入配置）
2. Project 节点字段：
- `name`
- `root_path`
- `created_at`
- `last_active_at`
3. Session 节点字段：
- `title`
- `pinned`
- `status`
- `agent_type`（codex / claude / shell）
- `updated_at`
4. Resources 节点子项：
- Snippets 列表
- Env 列表
- Files 列表

## 6. 功能需求（V1）

### FR-1 项目与会话管理

1. 用户可创建项目并绑定本地目录。
2. 用户可在一个项目下创建多个并行 Session。
3. Session 可重命名、关闭、删除。

### FR-2 Pin 标签页

1. Session 支持 `Pin/Unpin`。
2. `Pinned` 会话不可被“关闭全部”“关闭其他”误关。
3. 关闭单个 `Pinned` 会话时必须二次确认。

### FR-3 AI 状态可视化

1. 每个 Session 必须有状态字段：`idle/loading/need_approve/running/done/error`。
2. Sidebar 节点展示图标 + 颜色，状态变化实时更新。
3. 状态来源优先事件协议字段；文本匹配仅作兜底。

### FR-4 会话持久化与恢复

1. 应用退出时持久化以下数据：
- 项目与 Session 树结构
- 每个 Session 的窗口布局与最后活跃 pane
- 日志索引与最后滚动位置
- AI 会话元数据（session_id、最后状态、最后一条提示）
2. 应用重启后可一键恢复上次工作区。
3. 恢复后默认状态为“可继续”，但 `不自动续跑` 未完成任务。
4. 若后端支持 resume（如 codex resume），只提供手动触发入口。

### FR-5 Sidebar 资源

1. Snippets 支持新增、编辑、删除、一键插入当前终端。
2. Env 支持新增、编辑、删除、作用域绑定（全局/项目）。
3. Files 支持保存纯文本内容并快速打开到编辑视图。
4. V1 明文存储，UI 标注“未加密，仅本机文件保护”。

### FR-6 背景图自定义

1. 支持选择本地图片作为终端背景。
2. 支持设置背景填充策略（fill / fit / stretch，默认 fill）。
3. 支持设置背景透明度（建议默认 0.18~0.24 覆盖层）。

### FR-7 自动可读性适配（核心）

1. 自动模式下，系统持续根据背景亮度计算前景方案：
- 浅背景优先深色文字（黑系）
- 深背景优先浅色文字（白系）
2. 切换策略：
- 文字颜色在黑/白两套主题间切换
- 字重在 `normal` 与 `semibold` 间切换
3. 防抖策略：
- 使用滞回阈值避免频繁抖动（例如亮度高阈值与低阈值分离）
4. 可读性兜底：
- 若实时对比度低于阈值，自动提升遮罩强度或字重
5. 参数冻结（2026-04-01）：
- 亮度阈值：`L > 0.60` 切黑字，`L < 0.45` 切白字，`0.45 <= L <= 0.60` 保持当前颜色
- 字重策略：对比度不足时 `semibold`，其余 `normal`
- 对比度底线：目标 `>= 4.5:1`
- 更新时机：背景变更、窗口尺寸变更、滚动后重采样（节流）

## 7. 状态机（V1）

1. `idle`：会话空闲
2. `loading`：Agent 初始化或请求发出，尚未开始流式输出
3. `running`：任务执行中
4. `need_approve`：等待用户批准
5. `done`：任务成功完成
6. `error`：任务失败或中断

状态迁移（核心）：
1. `idle -> loading -> running`
2. `running -> need_approve -> running`
3. `running -> done`
4. `running -> error`
5. `need_approve -> error`（超时或拒绝）

## 8. 数据与存储（V1）

建议本地目录（可在实现时微调）：

- `~/.kaku/projects.json`
- `~/.kaku/projects/<project_id>/sessions.json`
- `~/.kaku/projects/<project_id>/resources/snippets.json`
- `~/.kaku/projects/<project_id>/resources/env.json`（明文）
- `~/.kaku/projects/<project_id>/resources/files/*.md`
- `~/.kaku/projects/<project_id>/ui/background.json`

约束：
1. 写入采用原子替换，避免异常退出导致 JSON 损坏。
2. 关键文件损坏时允许“降级启动 + 提示修复”。

## 9. 非功能要求

1. 启动恢复时间：
- 50 个 Session 以内，恢复入口可交互时间 < 2 秒（目标值）。
2. UI 响应：
- 状态变更到 Sidebar 呈现延迟 < 150ms（目标值）。
3. 背景适配性能：
- 自动配色计算不导致明显输入卡顿，平均额外开销可忽略。

## 10. 验收标准（V1）

1. 可以创建项目并在项目下并行创建多个 Session。
2. Pin 后执行“关闭其他/关闭全部”不会误关被 Pin 的 Session。
3. 重启应用后可恢复上次项目树与 Session 现场。
4. 恢复后不会自动继续执行未完成任务。
5. Sidebar 能管理 snippets/env/files 并可立即使用。
6. 设置背景图后，文字可在黑/白方案间自动切换，且可感知字重变化。
7. 在明暗反差较大的背景图下，终端文本保持可读。

## 11. 风险与后续

1. 明文 Env 存储存在安全风险，V1 接受，V1.5 或 V2 必须引入加密/Keychain。
2. 自动配色可能出现边缘场景误判，需保留手动覆盖开关。
3. 可编辑 Diff 是结构性复杂功能，单独规划到 V1.5，避免拖慢 V1 交付。
