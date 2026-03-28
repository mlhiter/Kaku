# Kaku Remote – iOS App

远程控制已运行的桌面 Kaku 终端。

## 快速开始

### 1. 在 Kaku 配置中启用 remote bridge

编辑 `~/.config/kaku/kaku.lua`：

```lua
config.remote = {
  enabled = true,
  port = 9988,
  bind = "0.0.0.0",
}
```

重启 Kaku，日志中会显示类似：
```
kaku-remote: starting on 0.0.0.0:9988 token=a1b2c3d4e5f6...
```

记录这个 token。

### 2. 在 Xcode 中打开 iOS 项目

```bash
open kaku-ios/Package.swift
```

Xcode 会自动解析 SwiftPM 依赖（SwiftTerm）。

### 3. 连接

1. 打开 KakuRemote App
2. 填入 Mac 的局域网 IP（`ifconfig` 查看）、端口 `9988`、token
3. 点击 Connect → 选择 pane → 开始控制

## 文件结构

```
Sources/KakuRemote/
├── App.swift               # @main 入口
├── ConnectionView.swift    # 连接配置 UI
├── PaneListView.swift      # pane 选择列表
├── SessionView.swift       # 终端会话主视图
├── TerminalView.swift      # SwiftTerm UIViewRepresentable 包装
├── KakuWebSocketClient.swift  # URLSessionWebSocketTask 封装 + REST API
└── KakuTheme.swift         # 从 /api/config 解析 Kaku 配色
```

## 验证

```bash
# 确认 HTTP 端点正常
curl http://<mac-ip>:9988/api/panes

# 测试 WebSocket（需要安装 wscat）
wscat -c ws://<mac-ip>:9988/ws/1 -H "x-kaku-token: <token>"
```
