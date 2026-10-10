# QuickPath 🚀

![index](https://raw.githubusercontent.com/clhome/QuickPath/main/Document/imgs/Image_index.webp)

<div align="center">

**简体中文** | [English](README.md)

[![Platform](https://img.shields.io/badge/Platform-Windows%2010%20%7C%2011-0078D6?logo=windows)](https://microsoft.com)
[![Rust](https://img.shields.io/badge/Language-Rust%202021-DEA584?logo=rust)](https://www.rust-lang.org)
[![Version](https://img.shields.io/badge/Version-v1.1.1-success)](#)
[![License](https://img.shields.io/badge/License-GPL--3.0-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/Website-qp.yftec.top-4A90E2)](https://qp.yftec.top)

**专为 Windows 10 与 Windows 11 深度打造的现代 Fluent 风格文件对话框智能路径跟随与快速跳转利器。**

[🌐 官方网站](https://qp.yftec.top) · [📖 详细使用说明书](Document/User_Manual_CN.md) · [📘 English Manual](Document/User_Manual_EN.md) · [🐛 问题反馈](https://github.com/clhome/QuickPath/issues)

</div>

---

## 💡 为什么选择 QuickPath？

在日常使用电脑时，您是否常常遇到这样的繁琐场景：
- 刚刚在**文件资源管理器**或 **Directory Opus** 中找到了深层工作目录，打开文本编辑器、IDE、Photoshop、Office 或 **WPS** 点击“另存为”或“打开文件”时，对话框却停留在默认的“下载”或“文档”目录；
- 您不得不一层层重新点选繁琐的目录树，或者手动复制文件管理器地址栏路径再粘贴到对话框中；
- 某些老旧辅助工具在粘贴路径时经常误触发中文输入法，导致文件名被拼音顶词覆盖破坏。

**QuickPath** 专为终结这一痛点而生！它能在文件对话框弹出的瞬间，毫秒级感知你正在使用的文件管理器目录，实现无感自动化秒切与悬浮直达，让你行云流水地专注于创作与工作。

---

## 🌟 核心特性

![detail](https://raw.githubusercontent.com/clhome/QuickPath/main/Document/imgs/detail.webp)

- **⚡ 智能自动秒切 (AutoSwitch)**：
  在文本编辑器、IDE、办公或设计软件中弹出“打开”或“另存为”对话框时，毫秒级无感自动同步至你最后浏览的文件管理器目录，彻底告别在长目录树中重复点选和复制粘贴。
- **📑 WPS Office 深度适配 (Ver 1.1.1 新增)**：
  攻克 WPS（文字、表格、演示、PDF 全套件，全面涵盖个人版与企业版）基于 Qt 框架自绘无原生 Win32 控件句柄文件对话框的行业难题。建立主窗口特征否定排除机制，杜绝主编辑界面误判；结合 Windows 原生 UI Automation (UIA) 启发式多维评分算法与安全剪贴板原子注入驱动，毫秒级感知 WPS 另存为/打开弹窗并实现瞬时秒切跳转，100% 完整保留用户敲入的文件名与扩展名。
- **📊 任务栏硬件状态监控 (Ver 1.1.0 之后新增)**：
  超轻量内置原生任务栏硬件看板，实时显示上传/下载网速、处理器 (CPU)、物理内存 (RAM)、独立显卡 (GPU) 及磁盘 I/O。两行自适应紧凑排布，支持 ClearType 锐利文字、微型指示条与三档阈值警示着色（绿/黄/红）。提供“任务栏右侧”与“任务栏左上方独立悬浮”（物理防遮挡应用标签）双停靠模式，具备全屏游戏/影音主动隐匿避让机制；模块启用时增量常驻内存 $\le 5\text{MB}$，停用时完全退出 0 开销。
- **🪟 Windows 11 现代 Fluent Design**：
  原生适配 Windows 11 **Mica / Acrylic（亚克力磨砂）** 材质与系统级深色/浅色自适应圆角，告别传统老旧丑陋的 Win32 弹窗。
- **📑 Windows 11 多标签页（Tabs）原生适配**：
  突破传统 COM 无法获取活动标签页的限制，精准感知 Windows 11 文件资源管理器的活跃 Tab。
- **🛡️ 输入法隔离与文件名保护 (ImeGuard)**：
  独创 `ImeGuard` 输入法状态保护机制，在路径注入过程中暂时挂起中文输入法候选，杜绝拼音顶词与乱码，同时智能保留用户原本敲入的文件名。
- **🔌 主流文件管理器全生态支持**：
  - Windows 11 / Windows 10 原生文件资源管理器 (Explorer，含多标签页)
  - Directory Opus (DOpus)
  - Total Commander (TC)
  - XYplorer
- **💼 办公与创作生态全兼容**：
  - WPS Office (文字/表格/演示/PDF 全套件，个人版与企业版，**Ver 1.1.1 深度适配**)
  - Microsoft Office (Word, Excel, PowerPoint)
  - 常用 IDE 与编辑器 (VS Code, Visual Studio, JetBrains 系列, Sublime Text, Notepad++ 等)
  - 图像设计套件 (Photoshop 等)
- **🚀 免 UAC 静默开机自启**：
  内置 Windows 任务计划程序管理，支持开机以最高权限静默启动，与任何以管理员权限运行的软件无缝通信，开机零弹窗打扰。
- **🎈 现代悬浮吸附条 (Floating Bar)**：
  贴合在文件对话框边缘，快捷键随时呼出（默认 `Ctrl + Q`），支持键盘上下键直选、收藏夹与历史记录瞬间直达。
- **🎨 丰富个性化定制**：
  支持 40% ~ 100% 窗口半透明度滑块无级调节、图形化按键录制自定义全局热键、中英文等多语言无缝切换（支持外部 TOML 扩展包）。
- **🪶 极速与超轻量**：
  纯原生 Rust 构建，单个独立绿色便携式可执行文件仅 **1.2 MB**，常驻后台内存 **< 10 MB**，待机 CPU 占用 **0%**。

---

## ⌨️ 常用快捷操作

| 动作 / 按键 | 功能说明 |
| :--- | :--- |
| **自动跟随** | 在文件管理器查看文件夹后，切入“打开/另存为”对话框，系统自动秒切到该路径 |
| **`Ctrl + Q`** (可自定义) | 在文件对话框中呼出现代路径选择悬浮栏 |
| **`↑` / `↓`** | 在悬浮候选列表（打开的窗口/最近历史/常用目录）中切换高亮项 |
| **`Enter`** | 确认并将选中的路径注入到当前对话框中瞬时跳转 |
| **`Esc`** | 关闭悬浮栏 |
| **监控面板左键双击** | 快速唤起 Windows 任务管理器 (`taskmgr.exe`) *(Ver 1.1.0 之后新增)* |
| **监控面板鼠标悬停** | 呼出 Fluent 硬件详情看板 (Tooltip)，展开双计量网速、Top 1 进程、独显专有显存、运行时间与电池状态 *(Ver 1.1.0 之后新增)* |
| **监控面板右键单击** | 弹出原生菜单：切换显示指标 (网速/CPU/RAM/GPU/Disk)、波形背景、停靠位置、刷新网卡适配器、直达设置中心 *(Ver 1.1.0 之后新增)* |
| **托盘图标单击 / 双击** | 打开现代卡片式设置中心 |
| **托盘图标右键** | 快捷菜单：切换自动秒切、任务栏监控开关、开机自启、呼出候选、打开设置或退出 |

---

## 🛠️ 设置中心一览

![settings](https://raw.githubusercontent.com/clhome/QuickPath/main/Document/imgs/settings.webp)

QuickPath 配备了符合 Fluent 视觉的现代化双选项卡卡片式设置中心：
- **常规自动化**：一键开启或关闭“智能自动秒切”，并提供滑块调节切换触发延时（40ms ~ 500ms）；
- **悬浮条外观**：自绘透明度滑动条，实时预览 40% ~ 100% 亚克力半透明磨砂效果；
- **任务栏硬件监控 (Ver 1.1.0 之后新增)**：一键开启/停用状态监控、选择停靠位置（任务栏右侧 / 任务栏左上方独立悬浮）、细粒度勾选监控指标（网速/CPU/RAM/GPU/Disk）、切换 15~20秒实时历史波形图背景、选择采样刷新率（1.0s / 3.0s 推荐 / 5.0s）及调节看板透明度（50% ~ 100%）；
![hardwareMonitor](https://raw.githubusercontent.com/clhome/QuickPath/main/Document/imgs/hardwareMonitor.webp)

- **自定义快捷键**：图形化点击录制模式，支持任意组合键（Ctrl / Alt / Shift / Win + 按键）动态热注册生效；
- **系统自启动**：智能创建免 UAC 任务计划，开机静默启动；
- **多语言切换**：支持简体中文、English 及加载外部 `./locales/*.toml` 语言包；
- **关于与支持**：展示版本信息、官网直达超链接与 GitHub 开源仓库入口。

---

## ⚙️ 配置文件说明

QuickPath 采用绿色双模式设计：
1. **便携模式**：若程序同级目录下存在 `quickpath.json`，则所有配置就地保存（适合放在 U 盘或移动硬盘）。
2. **标准模式**：默认持久化保存于 `%APPDATA%\QuickPath\config.json`。

配置项示例：
```json
{
  "auto_switch_enabled": true,
  "auto_switch_delay_ms": 80,
  "hotkey": "Ctrl+Q",
  "floating_bar_enabled": true,
  "floating_bar_auto_show": false,
  "floating_bar_opacity": 90,
  "autostart_enabled": false,
  "autostart_task_scheduler": true,
  "language": "zh-CN",
  "blacklist_processes": [
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe"
  ],
  "pinned_folders": [],
  "history_folders": [],
  "monitor": {
    "enabled": true,
    "position": "TrayLeft",
    "show_network": true,
    "show_cpu": true,
    "show_memory": true,
    "show_gpu": true,
    "show_disk": true,
    "show_graph_bg": true,
    "refresh_interval_ms": 3000,
    "opacity": 95
  }
}
```

> **说明 (Ver 1.1.0 之后新增)**：`monitor` 字段负责任务栏硬件监控配置，其中 `position` 支持 `"TrayLeft"`（任务栏右侧）与 `"TaskbarLeft"`（任务栏左上方独立悬浮）；`refresh_interval_ms` 支持 `1000`、`3000`（默认推荐）与 `5000` 毫秒。各指标项均支持布尔值细粒度独立开关。
---

## 🏗️ 源码构建与编译

本项目基于标准 Rust 工具链开发，支持 Windows 10 (1809+) 及 Windows 11。

### 前置环境
- 安装 [Rust 官方工具链](https://www.rust-lang.org/) (建议使用 stable 通道)
- Windows 10 (1809 及以上) 或 Windows 11

### 编译步骤
```pwsh
# 1. 克隆代码仓库
git clone https://github.com/clhome/QuickPath.git
cd QuickPath

# 2. 调试运行
cargo run

# 3. 构建发布版（高阶体积与性能优化）
cargo build --release
```

编译产物位于 `target/release/quickpath.exe`，为一个独立无依赖的单文件绿色可执行程序。

---

## 📚 详细文档

想要深入了解 QuickPath 的详细用法、生态适配配置和进阶技巧，请查阅官方手册：
- 🇨🇳 [中文用户使用说明书 (User Manual)](Document/User_Manual_CN.md)
- 🇺🇸 [English User Manual & Technical Guide](Document/User_Manual_EN.md)

---

## 🏢 出品与致谢

- **出品方**：衢州御风科技有限公司 (Quzhou Yufeng Technology Co., Ltd.)
- **官方网址**：[https://qp.yftec.top](https://qp.yftec.top)
- **代码仓库**：[https://github.com/clhome/QuickPath](https://github.com/clhome/QuickPath)

---

## 📄 开源许可证

本项目基于 [GNU General Public License v3.0](LICENSE) 协议开源。
