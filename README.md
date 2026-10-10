# QuickPath 🚀

![index](https://raw.githubusercontent.com/clhome/QuickPath/main/Document/imgs/Image_index.webp)

<div align="center">

[简体中文](README_CN.md) | **English**

[![Platform](https://img.shields.io/badge/Platform-Windows%2010%20%7C%2011-0078D6?logo=windows)](https://microsoft.com)
[![Rust](https://img.shields.io/badge/Language-Rust%202021-DEA584?logo=rust)](https://www.rust-lang.org)
[![Version](https://img.shields.io/badge/Version-v1.1.1-success)](#)
[![License](https://img.shields.io/badge/License-GPL--3.0-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/Website-qp.yftec.top-4A90E2)](https://qp.yftec.top)

**A modern Fluent-style smart path following and quick navigation companion for Windows 10 & 11 file dialogs.**

[🌐 Official Website](https://qp.yftec.top) · [📘 User Manual (English)](Document/User_Manual_EN.md) · [📖 使用说明书 (中文)](Document/User_Manual_CN.md) · [🐛 Issues & Feedback](https://github.com/clhome/QuickPath/issues)

</div>

---

## 💡 Why QuickPath?

When working across multiple applications, do you frequently face this frustration?
- You just navigated to a deeply nested project folder in **File Explorer** or **Directory Opus**, but when invoking "Open" or "Save As" in your code editor, IDE, Photoshop, Office, or **WPS Office**, the file dialog defaults back to "Downloads" or "Documents";
- You are forced to traverse through long directory trees again or manually copy and paste folder paths into the dialog address bar;
- Outdated legacy switcher tools often trigger your Input Method Editor (IME) unintentionally during path injection, causing typos, candidate popups, or accidentally overwriting your desired filename.

**QuickPath** was engineered to eliminate this friction once and for all. It senses your active file manager folders in milliseconds upon opening a file dialog, providing seamless automatic synchronization and instant keyboard-driven navigation.

---

## 🌟 Key Features

![detail](https://raw.githubusercontent.com/clhome/QuickPath/main/Document/imgs/detail_en.webp)

- **⚡ Smart AutoSwitch**:
  When an "Open" or "Save As" dialog appears in editors, IDEs, or creative software, QuickPath synchronizes it to your most recently browsed file manager directory within milliseconds—eliminating repetitive path hunting and copy-pasting.
- **📑 WPS Office Deep Adaptation (New in Ver 1.1.1)**:
  Overcomes the longstanding technical barrier of intercepting custom Qt-based owner-drawn file dialogs in WPS Office (Writer, Spreadsheets, Presentation, and PDF across both Personal and Enterprise editions). Features an intelligent main window negation filter to avoid false triggers, combined with native Windows UI Automation (UIA) heuristic scoring and atomic clipboard injection to achieve instant folder switching while flawlessly preserving the user's typed filename and extension.
- **📊 Taskbar Hardware Status Monitor (New in Ver 1.1.0)**:
  Ultra-lightweight native hardware dashboard docked right on your Windows taskbar. Displays real-time upload/download speeds, CPU, RAM, dedicated GPU, and Disk I/O across an adaptive 2-row layout. Features crisp Win32 ClearType typography, mini indicator bars, and 3-tier dynamic threshold coloring (Green/Yellow/Red). Supports dual docking modes ("Right of Taskbar" or "Above Taskbar Left Float" to prevent blocking taskbar app buttons) and automatic stealth evasion during full-screen games/media. Consumes $\le 5\text{MB}$ additional RAM when active and drops to absolute 0% CPU & 0 MB footprint when disabled.
- **🪟 Windows 11 Modern Fluent Design**:
  Natively integrates Windows 11 **Mica / Acrylic blur**, rounded corners, and adaptive dark/light themes. Say goodbye to dated and clunky legacy Win32 popups.
- **📑 Windows 11 Multi-Tab Explorer Support**:
  Overcomes the classic COM API limitation that fails to detect active tabs in Windows 11 Explorer, accurately recognizing the currently focused tab.
- **🛡️ IME Guard & Filename Preservation (ImeGuard)**:
  Innovative input method protection mechanism temporarily suspends IME candidate windows during path injection, preventing pinyin typos and garbled text while intelligently preserving filenames you've already typed.
- **🔌 Full File Manager Ecosystem Support**:
  - Windows 11 / 10 Native File Explorer (including multi-tab recognition)
  - Directory Opus (DOpus)
  - Total Commander (TC)
  - XYplorer
- **💼 Comprehensive Productivity & Creative Suite Compatibility**:
  - WPS Office (Writer, Spreadsheets, Presentation, PDF, Personal & Enterprise editions, **New in Ver 1.1.1**)
  - Microsoft Office (Word, Excel, PowerPoint)
  - Popular IDEs & Code Editors (VS Code, Visual Studio, JetBrains IDEs, Sublime Text, Notepad++, etc.)
  - Creative Applications (Photoshop, Illustrator, etc.)
- **🚀 UAC-Free Silent AutoStart**:
  Built-in Windows Task Scheduler integration allows the application to launch silently with highest privileges at system startup, eliminating annoying UAC prompts while communicating seamlessly with elevated applications.
- **🎈 Modern Floating Bar**:
  Snaps to the edge of your file dialogs. Summon it instantly via hotkey (default `Ctrl + Q`), navigate candidates with `↑`/`↓` arrow keys, and jump to pinned favorites or recent folders with a single press of `Enter`.
- **🎨 Deep Customization**:
  Adjust window transparency seamlessly from 40% to 100% via a sleek slider, record custom global hotkeys interactively, and switch between English, Simplified Chinese, or custom external TOML language packs.
- **🪶 Ultra-Lightweight & Blazing Fast**:
  Built purely with native Rust. The standalone portable binary is only **~1.2 MB**, consumes **< 10 MB** of background memory, and uses **0%** idle CPU.

---

## ⌨️ Common Shortcuts & Controls

| Action / Hotkey | Description |
| :--- | :--- |
| **Auto Follow** | Browse a folder in your file manager; when an "Open/Save" dialog appears, QuickPath switches to it automatically |
| **`Ctrl + Q`** (Customizable) | Summon the modern path picker floating bar adjacent to the active file dialog |
| **`↑` / `↓`** | Navigate between active file manager tabs, pinned favorites, and recent history |
| **`Enter`** | Confirm and instantly inject the selected path into the file dialog |
| **`Esc`** | Dismiss the floating bar |
| **Monitor Double-Click** | Instantly launch Windows Task Manager (`taskmgr.exe`) *(New in Ver 1.1.0)* |
| **Monitor Mouse Hover** | Summon Fluent Hardware Dashboard (Tooltip) showing dual-unit network speed, top consumer process, dedicated VRAM, uptime, and battery *(New in Ver 1.1.0)* |
| **Monitor Right-Click** | Native context menu: toggle metrics (Net/CPU/RAM/GPU/Disk), waveform background, dock position, refresh network, open settings *(New in Ver 1.1.0)* |
| **Tray Icon (Left Click / Double Click)** | Open the modern card-style Settings Center |
| **Tray Icon (Right Click)** | Context menu: toggle AutoSwitch, toggle Taskbar Monitor, toggle AutoStart, summon bar, open settings, or exit |

---

## 🛠️ Modern Settings Center

![settings](https://raw.githubusercontent.com/clhome/QuickPath/main/Document/imgs/settings_en.webp)

QuickPath includes a modern Fluent dual-tab card-based Settings Center:
- **General Automation**: Toggle "Smart AutoSwitch" with a single click, and adjust switch response delay (40ms – 500ms).
- **Appearance & Opacity**: Intuitive opacity slider supporting real-time preview between 40% and 100% acrylic transparency.
- **Taskbar Hardware Monitor (New in Ver 1.1.0)**: Master toggle for hardware monitoring, docking position selector ("Right of Taskbar" / "Above Taskbar Left Float"), granular metric toggles (Network/CPU/RAM/GPU/Disk), 15~20s historical waveform graph toggle, sampling refresh rate selector (1.0s / 3.0s recommended / 5.0s), and monitor panel transparency slider (50% – 100%).

![hardwareMonitor](https://raw.githubusercontent.com/clhome/QuickPath/main/Document/imgs/hardwareMonitor.webp)
- **Custom Hotkey**: Interactive key recording interface supporting custom modifier combinations (Ctrl, Alt, Shift, Win) with dynamic hotkey re-registration.
- **System Startup**: Seamless Task Scheduler integration for UAC-free elevation on boot.
- **Language Switcher**: Dynamic switching between English, Simplified Chinese, or any external `./locales/*.toml` files.
- **About & Community**: View version info, open the official website, or visit the GitHub repository.

---

## ⚙️ Configuration File

QuickPath features dual-mode persistence:
1. **Portable Mode**: If `quickpath.json` exists in the same directory as the executable, all settings are saved locally (ideal for USB drives).
2. **Standard Mode**: Defaults to `%APPDATA%\QuickPath\config.json`.

Sample configuration:
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
  "language": "en-US",
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

> **Note (New in Ver 1.1.0)**: The `monitor` section configures the taskbar hardware status monitor. `position` accepts `"TrayLeft"` (Right of Taskbar) or `"TaskbarLeft"` (Floating Above Taskbar Left). `refresh_interval_ms` supports `1000`, `3000` (default recommended), and `5000` ms. Each metric toggle is independently configurable.

---

## 🏗️ Building from Source

QuickPath is built with the standard Rust toolchain and targets Windows 10 (1809+) and Windows 11.

### Prerequisites
- [Rust Toolchain](https://www.rust-lang.org/) (Stable channel recommended)
- Windows 10 (1809 or higher) / Windows 11

### Build Steps
```pwsh
# 1. Clone the repository
git clone https://github.com/clhome/QuickPath.git
cd QuickPath

# 2. Run in development mode
cargo run

# 3. Build optimized release binary
cargo build --release
```

The output binary will be located at `target/release/quickpath.exe`. It is a single, zero-dependency, portable executable.

---

## 📚 Detailed Documentation

For in-depth usage scenarios, ecosystem configurations, and advanced customization, refer to our comprehensive manuals:
- 📘 [English User Manual & Technical Guide](Document/User_Manual_EN.md)
- 🇨🇳 [中文用户使用说明书 (User Manual)](Document/User_Manual_CN.md)

---

## 🏢 Producer & Acknowledgments

- **Produced by**: Quzhou Yufeng Technology Co., Ltd. (衢州御风科技有限公司)
- **Official Website**: [https://qp.yftec.top](https://qp.yftec.top)
- **GitHub Repository**: [https://github.com/clhome/QuickPath](https://github.com/clhome/QuickPath)

---

## 📄 License

This project is licensed under the [GNU General Public License v3.0](LICENSE).
