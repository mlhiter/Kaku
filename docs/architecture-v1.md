# Kaku 二开架构设计（V1）

- 文档日期：2026-04-01
- 对应需求：`docs/requirements-v1.md`
- 状态：Draft（可进入技术拆解）

## 1. 设计目标

1. 支撑 `Project -> Sessions -> Resources` 的左侧工作台模型。
2. 将 AI 任务状态以统一状态机映射到 Sidebar/UI。
3. 提供稳定的本地持久化与重启恢复（仅恢复界面和上下文，不自动续跑）。
4. 提供背景图 + 自动可读性适配，保证文本可读。

## 2. 系统分层

1. `UI Layer`（kaku-gui）
- Sidebar 渲染（Project/Session/Resources）
- Session 状态徽标渲染
- 背景图设置与可读性适配应用

2. `Domain Layer`
- ProjectManager：项目生命周期管理
- SessionManager：会话创建、销毁、Pin、状态变更
- ResourceManager：Snippets / Env / Files 管理
- ThemeManager：背景图与自动可读性策略
- RestoreManager：快照持久化和恢复流程

3. `Integration Layer`
- AgentAdapter：Codex / Claude / Shell 事件适配
- WezTermBridge：Tab/Pane 与 Session 映射、命令执行桥接
- EventBus：统一事件分发与订阅

4. `Storage Layer`
- 本地 JSON + 文本文件存储
- 原子写入与损坏降级恢复

## 3. 核心数据模型

### 3.1 Project

```json
{
  "id": "proj_xxx",
  "name": "my-project",
  "root_path": "/abs/path",
  "created_at": "2026-04-01T10:00:00Z",
  "last_active_at": "2026-04-01T12:00:00Z"
}
```

### 3.2 Session

```json
{
  "id": "sess_xxx",
  "project_id": "proj_xxx",
  "title": "fix-auth-bug",
  "agent_type": "codex",
  "pinned": true,
  "status": "running",
  "pane_ref": "wezterm-pane-id",
  "tab_ref": "wezterm-tab-id",
  "last_scroll_offset": 1280,
  "resume_hint": {
    "provider": "codex",
    "session_id": "abc123",
    "available": true
  },
  "updated_at": "2026-04-01T12:05:00Z"
}
```

### 3.3 Resources

```json
{
  "snippets": [
    {
      "id": "snip_xxx",
      "name": "quick commit",
      "content": "git add -A && git commit -m \"msg\""
    }
  ],
  "env": [
    {
      "id": "env_xxx",
      "scope": "project",
      "key": "OPENAI_API_KEY",
      "value": "plaintext-for-v1"
    }
  ],
  "files": [
    {
      "id": "file_xxx",
      "name": "todo.md",
      "path": "resources/files/todo.md"
    }
  ]
}
```

### 3.4 ThemeState

```json
{
  "background": {
    "image_path": "/abs/path/wallpaper.jpg",
    "fit_mode": "fill",
    "overlay_alpha": 0.22
  },
  "readability": {
    "mode": "auto",
    "current_fg": "light",
    "current_weight": "normal",
    "last_luminance": 0.41,
    "last_contrast_ratio": 5.2
  }
}
```

## 4. 状态机与事件流

### 4.1 Session 状态机

状态集合：
1. `idle`
2. `loading`
3. `running`
4. `need_approve`
5. `done`
6. `error`

核心迁移：
1. `idle -> loading -> running`
2. `running -> need_approve -> running`
3. `running -> done`
4. `running -> error`
5. `need_approve -> error`

### 4.2 事件总线（推荐事件名）

1. `project.created`
2. `project.removed`
3. `session.created`
4. `session.updated`
5. `session.pinned_changed`
6. `session.status_changed`
7. `agent.output`
8. `agent.approval_required`
9. `agent.completed`
10. `agent.failed`
11. `theme.background_changed`
12. `theme.readability_updated`
13. `restore.snapshot_saved`
14. `restore.snapshot_loaded`

### 4.3 状态映射规则

1. 收到 `agent.approval_required` 立即切 `need_approve`。
2. 收到 `agent.completed` 置 `done`，并记录完成时间。
3. 收到 `agent.failed` 置 `error`，附错误摘要。
4. 文本匹配仅用于兜底，不覆盖结构化状态事件。

## 5. 持久化与恢复架构

### 5.1 存储布局

1. `~/.kaku/projects.json`
2. `~/.kaku/projects/<project_id>/sessions.json`
3. `~/.kaku/projects/<project_id>/resources/snippets.json`
4. `~/.kaku/projects/<project_id>/resources/env.json`（明文）
5. `~/.kaku/projects/<project_id>/resources/files/*.md`
6. `~/.kaku/projects/<project_id>/ui/background.json`

### 5.2 写入策略

1. 所有 JSON 先写临时文件，再 `rename` 原子替换。
2. 高频状态更新批量落盘（例如 300ms debounce）。
3. 退出流程强制 flush 一次快照。

### 5.3 恢复语义

1. 启动后恢复 Project/Session 树和 UI 布局。
2. 恢复日志位置、元数据、最后状态。
3. 不自动重启任何命令或 AI 任务。
4. 若 `resume_hint.available = true`，提供显式“Resume”按钮手动触发。

## 6. Pin 语义实现

1. `pinned=true` 的 Session 不参与批量关闭。
2. 手动关闭 pinned Session 时显示确认框。
3. 删除 Project 时可提供“包含 pinned 一并删除”二次确认。

## 7. 背景图可读性适配（FR-7 落地）

### 7.1 采样与亮度

1. 从终端可视区域做降采样（例如 `32x32`）计算平均亮度 `L`。
2. 亮度计算使用相对亮度近似值（sRGB 转线性后加权）。

### 7.2 切换阈值（已冻结）

1. `L > 0.60`：前景切换为黑系。
2. `L < 0.45`：前景切换为白系。
3. `0.45 <= L <= 0.60`：保持当前前景不变（滞回区）。

### 7.3 字重与对比度

1. 对比度目标：`>= 4.5:1`。
2. 若低于目标，字体从 `normal` 升级为 `semibold`。
3. 若仍低于目标，提升遮罩透明度（上限可设 `0.36`）。

### 7.4 更新触发与节流

1. 背景变更：立即重算。
2. 窗口尺寸变更：`100ms` trailing throttle 重算。
3. 滚动事件：`150ms` trailing throttle 重算。
4. 连续重算仅在计算结果变化时触发 UI 更新，避免抖动。

## 8. 模块接口建议

1. `SessionManager::update_status(session_id, status, reason)`
2. `SessionManager::set_pinned(session_id, pinned)`
3. `RestoreManager::save_snapshot()`
4. `RestoreManager::load_snapshot()`
5. `ThemeManager::set_background(image_path, fit_mode)`
6. `ThemeManager::recompute_readability(trigger)`
7. `ResourceManager::upsert_env(project_id, key, value, scope)`

## 9. 失败与降级策略

1. 背景图读取失败：回退默认纯色背景并提示。
2. Readability 算法异常：回退静态主题（白字方案）。
3. 会话快照损坏：保留可解析部分并提示用户恢复失败项。
4. Agent 事件缺失：状态回退 `running`，并标记“状态不可信”。

## 10. 里程碑建议

1. M1：Sidebar + Project/Session + Pin + 基础持久化。
2. M2：Agent 状态总线 + 状态徽标 + 恢复语义闭环。
3. M3：Resources（snippets/env/files）+ 背景图与自动可读性。
4. M4：稳定性打磨（恢复、性能、降级路径、验收测试）。

