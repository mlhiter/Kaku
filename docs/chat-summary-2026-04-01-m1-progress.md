# Kaku 开发快照（M1 收尾）

- 日期：2026-04-01
- 目的：在 compact 上下文后可无缝继续开发

## 1. 当前里程碑状态

M1（Sidebar + Project/Session + Pin + 基础持久化）已基本完成并可运行。

已完成能力：
1. Project/Session 本地持久化（`~/.kaku/projects.json` + `sessions.json`）
2. Session 的 Pin/Unpin 与关闭保护
3. Session 重命名、删除（含确认）
4. Session 状态徽标渲染（I/L/?/R/D/E/U）
5. Sidebar 可调宽并持久化宽度
6. Session 行可点击激活，不再崩溃
7. Session 右键菜单（Open/Rename/Pin/Close Others/Delete）
8. Sidebar 外露操作项已隐藏（只保留右键菜单入口）
9. `Close All Unpinned` 已从 sidebar UI 移除

## 2. 关键设计决策（已落地）

1. Session 重命名采用 GUI modal（支持中文输入法），不再使用终端 overlay 输入。
2. 右键不是系统原生菜单，而是自绘 modal 菜单（样式可控、跨平台一致）。
3. Session 的批量/危险操作通过菜单和确认框触发，减少 sidebar 文本噪声。

## 3. 用户手测结论（已通过）

1. Session 行可点击切换且不再卡死。
2. Sidebar 宽度可拖拽。
3. Pinned Session 关闭确认正常。
4. Delete Session 正常。
5. Rename Session 正常，弹窗位置与输入行为正常。
6. 右键菜单可打开，各项操作正常。
7. 右键菜单视觉问题已修复（去掉贴字背景层）。
8. Sidebar 外露操作项已消失，符合预期。

## 4. 近期提交（可作为恢复锚点）

1. `d03868f` feat(workspace): enhance session management commands in M1 workspace
2. `fee34a4` chore(.gitignore): add .local-rustup to ignore list
3. `0e0ceaa` feat(sidebar): implement context menu for session management in sidebar

## 5. 常用验证命令

1. `cargo check -p kaku-gui`
2. `cargo test -p kaku-gui render::sidebar::tests:: -- --nocapture`
3. `cargo build -p kaku-gui`
4. `cargo test -p kaku m1_workspace::tests:: -- --nocapture`

## 6. 本机环境注意事项

1. 本机 `~/.rustup` 可能存在权限问题（root 拥有），导致 `cargo` 无法写临时文件。
2. 如复现该问题，可临时使用：
   - `RUSTUP_HOME=$PWD/.local-rustup /Users/mlhiter/.cargo/bin/cargo ...`

## 7. 建议下一步（M2 起点）

按 `docs/architecture-v1.md` 的里程碑建议继续：
1. Agent 状态总线（事件驱动）
2. Session 状态与恢复语义闭环（重启后状态可信度策略）
3. 将状态来源从“文件渲染”推进到“运行时事件 + 持久化快照”统一

## 8. M1 未完成项记录（颜色相关）

1. `FR-6 背景图自定义` 尚未完成端到端实现与验收。
2. `FR-7 自动可读性适配（黑/白切换、字重、自适应对比度）` 尚未完成端到端实现与验收。
3. 当前仅有 Sidebar 的基础状态色映射（I/L/?/R/D/E/U）已实现，不能替代 FR-6/FR-7 的验收结论。

## 9. M1 交互收敛记录（2026-04-01）

1. 原生底部 Tab Bar 已隐藏，Sidebar 作为主导航入口。
2. `Actions` 区块已移除，不再作为新增入口。
3. 新增入口改为：
   - `Projects` 标题右侧 `+`：创建 Project（输入 root path）
   - Project 行右侧 `+`：创建 Session，并自动进入重命名弹窗
4. 重命名触发改为“双轨”：
   - 右键菜单 `Rename`
   - 双击 Project / Session 行
5. Session 状态展示调整：
   - Idle 不再展示 `I`，仅展示 pin 状态（`[📌]` 或 `[·]`）
   - 非 Idle 才展示状态字母（如 `[📌|R]`）
6. Project root path 创建支持 `~/...` 路径输入（展开到用户 home）。
7. Project 行支持右键菜单：`New Session`、`Rename Project`、`Delete Project`（带二次确认）。
8. Project 行 hover 显示双按钮：`+`（新建 Session）和 `⋯`（打开项目菜单），提升无右键设备可用性。

## 10. M1 快捷键增强（2026-04-02）

1. Sidebar UI 状态持久化增加 `visible` 字段（`~/.kaku/gui/sidebar.json`），支持记住显隐状态。
2. 新增快捷键：
   - `Cmd+Shift+B`：切换 Sidebar 显隐
   - `Cmd+Opt+N`：新建 Project
   - `Cmd+Opt+T`：在当前 Project 新建 Session
   - `Cmd+Opt+P`：Pin/Unpin 当前 Session
   - `Cmd+Opt+R`：重命名当前 Session
   - `Cmd+Opt+Backspace`：删除当前 Session（保留确认框）
3. 快捷键上下文解析规则：
   - 优先使用当前 active tab 绑定的 project/session
   - 无上下文时显示 toast，不执行危险默认行为

## 11. 快捷键帮助面板（2026-04-02）

1. 新增快捷键帮助弹层（modal）：`Cmd+Shift+/` 打开/关闭。
2. 帮助面板展示分组：
   - Sidebar 快捷键
   - Terminal & Window 快捷键
3. 帮助面板关闭方式：
   - `Esc`
   - `Enter`
   - `Cmd+Shift+/`
   - 点击面板外区域
4. 帮助面板样式修正：去除行级背景溢出问题（不再超出弹窗边界）。
5. 帮助面板内容扩展：覆盖 Sidebar、Tabs/Window、Pane/Tools、Editing 的常用快捷键。

## 12. 快捷键帮助改为 Tab 页（2026-04-02）

1. `Cmd+Shift+/` 从弹窗改为打开独立 Help Tab。
2. Help Tab 使用 `less` 展示完整快捷键列表（支持滚动查看，避免弹窗高度溢出）。
3. 若系统无 `less`，自动降级为普通文本输出并等待回车关闭。
