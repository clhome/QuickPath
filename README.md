# QuickPath 🚀

> **专为 Windows 10 与 Windows 11 深度打造的现代 Fluent 风格文件对话框智能路径跟随与快速跳转利器。**

---

## 🌟 核心特性

- **⚡ 智能自动秒切 (AutoSwitch)**：
  在文本编辑器、IDE、办公或设计软件中弹出“打开”或“另存为”对话框时，毫秒级无感自动同步至你最后浏览的文件管理器目录，彻底告别在长目录树中重复点选和复制粘贴。
- **🪟 Windows 11 现代 Fluent Design**：
  原生适配 Windows 11 **Mica / Acrylic（亚克力磨砂）** 材质与系统级深色/浅色自适应圆角，告别传统老旧丑陋的 Win32 弹窗。
- **📑 Windows 11 多标签页（Tabs）原生适配**：
  突破传统 COM 无法获取活动标签页的限制，精准感知 Windows 11 文件资源管理器的活跃 Tab。
- **🛡️ 输入法隔离与文件名保护**：
  独创 `ImeGuard` 输入法状态保护机制，在路径注入过程中暂时挂起中文输入法候选，杜绝拼音顶词与乱码，同时智能保留用户原本敲入的文件名。
- **🔌 主流文件管理器全生态支持**：
  - Windows 11 / Windows 10 文件资源管理器 (Explorer)
  - Directory Opus (DOpus)
  - Total Commander (TC)
  - XYplorer
- **🚀 免 UAC 静默开机自启**：
  内置 Windows 任务计划程序管理，支持开机以最高权限静默启动，与任何以管理员权限运行的软件无缝通信，开机零弹窗打扰。
- **🎈 现代悬浮吸附条 (Floating Bar)**：
  贴合在文件对话框边缘，快捷键 `Ctrl + Q` 随时呼出，支持上下键直选、收藏夹与历史记录直达。
- **🪶 极速与超轻量**：
  纯原生 Rust 构建，单个便携式可执行文件仅 **520 KB**，常驻后台内存 **< 10 MB**，待机 CPU 占用 **0%**。

---

## ⌨️ 常用快捷操作

| 动作 | 说明 |
| :--- | :--- |
| **自动跟随** | 在文件管理器查看文件夹后，切入“打开/另存为”对话框，系统自动秒切到该路径 |
| **`Ctrl + Q`** | 在文件对话框中呼出现代路径选择悬浮栏 |
| **`↑` / `↓`** | 切换候选列表中的路径 |
| **`Enter`** | 确认并将选中的路径注入到当前对话框中 |
| **`Esc`** | 关闭悬浮栏 |
| **托盘图标右键** | 快速切换自动秒切状态、开机自启开关、打开设置中心或退出 |
| **托盘图标左键** | 直接打开现代卡片式设置中心 |

---

## ⚙️ 配置文件说明

配置持久化保存于 `%APPDATA%\QuickPath\config.json`（或与可执行文件同级的 `quickpath.json` 便携模式）。

```json
{
  "auto_switch_enabled": true,
  "auto_switch_delay_ms": 80,
  "hotkey": "Ctrl+Q",
  "floating_bar_enabled": true,
  "floating_bar_auto_show": false,
  "autostart_enabled": false,
  "autostart_task_scheduler": true,
  "blacklist_processes": [
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe"
  ],
  "pinned_folders": [],
  "history_folders": []
}
```

---

## 🛠️ 构建与编译

本项目基于标准 Rust 工具链开发，支持 Windows 10 (1809+) 及 Windows 11。

```pwsh
# 调试运行
cargo run

# 构建最终优化发布版
cargo build --release
```

编译产物位于 `target/release/quickpath.exe`。
