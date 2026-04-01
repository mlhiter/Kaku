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

