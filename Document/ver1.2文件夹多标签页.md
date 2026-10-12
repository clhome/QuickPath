# QuickPath 资源管理器多标签页增强模块 (QuickPath-TabModule) RPD (需求与产品设计文档)

## 1. 模块定位与工程目标

### 1.1 模块定位

参考项目：https://github.com/w4po/ExplorerTabUtility，原项目内存占用过大（80MB），将原 C# 项目 `ExplorerTabUtility` 的核心逻辑彻底解构，使用纯 Rust 原生实现，作为 **QuickPath** 的内置子模块（或可常驻后台的独立轻量守护组件）。模块专注于 Windows 11 原生文件资源管理器的窗口全生命周期接管、智能多标签页合并、Tab 状态去重与历史栈维护，并深度协同 QuickPath 的路径跳转能力。

### 1.2 核心设计哲学

- **极致资源控制：** 杜绝 GC、消除运行时损耗、完全剥离 DirectX/XAML 等重型渲染管道。后台稳态常驻内存控制在 **5MB ~ 12MB** 以内，CPU 空载占用稳定为 **0.00%**。
- **KISS 原则：** 剔除原项目中冗余的自绘框架与厚重封装，仅保留最底层、确定性最高的 Win32 API、Shell COM 接口与 UI Automation 自动化管道。
- **低侵入与高安全性：** 不注入外部 DLL，避免触发杀毒软件误报；完全基于 Windows 辅助功能接口、Shell COM 协同以及标准系统钩子运行。
- **无缝感知体验：** 消除窗口捕获过程中的黑白屏闪烁，并对 IDE/浏览器的上下文关联动作实现 100% 的行为保真。
- **国际化与语言中立：** 原生支持中英文界面，底层引擎深度遵循语言中立（Language-Agnostic）原则，不受操作系统显示语言干扰。

## 2. 软硬件环境与兼容性基线

- **目标平台：** Windows 11 64位（x86_64 / aarch64）
- **语言支持：** 简体中文 (zh-CN) 与 英文 (en-US)，并支持跟随 Windows 系统语言自动切换
- **最低系统基准：** Windows 11 22H2 Build 22621.675+（必须具备包含原生 Tabs 的 XAML 架构文件资源管理器）
- **开发与编译规范：**
  - Rust Edition 2021+
  - 编码规范：UTF-8 无 BOM，LF 换行符
  - 测试隔离：所有测试探针、诊断批处理与验证脚本统一存放在根目录 `test/` 目录下
- **权限与特权边界：** 默认以中特权（普通用户权限）运行；若捕获到以管理员权限运行的提权资源管理器窗口，启用静默降级或提示 UAC 匹配策略，杜绝因 UIPI（用户界面特权隔离）导致的调用静默失败。

## 3. 功能特性规格说明

### 3.1 核心交互场景驱动覆盖

#### 场景 A：`Win + E` 呼出行为自适应

- **触发机制：** 用户按下系统全局快捷键 `Win + E`。
- **状态分流判定：**
  - `State: NoActiveWindow`（当前无存活的资源管理器窗口）：直接放行系统原生初始化流程，正常弹出首个主窗口。
  - `State: ActiveWindowExists`（当前已存在资源管理器宿主窗口）：
    1. 快速拦截系统派生的多余顶级窗口。
    2. 将请求转换为向当前聚焦（或最近激活）的宿主窗口发送“新建标签页”指令。
    3. 新标签页默认定向至系统预设的“主文件夹（Home）”或“此电脑（This PC）”。

#### 场景 B：外部程序（IDE / 浏览器 / 终端）触发“在文件夹中显示（Show in Folder）”

- **触发机制：** 外部第三方进程（如 VSCode、Cursor、Chrome、微信）通过 `explorer.exe /select,"C:\path\to\file.ext"` 命令行参数或底层直接调用 `SHOpenFolderAndSelectItems` 接口唤起资源管理器。
- **状态保持与高亮还原要求：**
  1. **元数据提取：** 拦截新窗口初始化上下文，提取其目标文件的父级目录（Parent Folder Path）及目标文件本身的 PIDL / 文件名。
  2. **容器分流：**
     - 若当前系统**无打开的资源管理器**：放行原生窗口创建，允许其自然聚焦并高亮文件。
     - 若当前系统**已有宿主窗口**：
       - 判断宿主窗口中是否已存在该父级目录的标签页。若存在，直接激活该 Tab；若不存在，新建 Tab 并导航至该父级目录。
  3. **选中态保真（Crucial）：** 目录就绪后，通过 `IShellView::SelectItem` 或 UI Automation 列表项聚焦定位，对目标文件注入 `SVSI_SELECT | SVSI_ENSUREVISIBLE | SVSI_FOCUSED` 状态，确保目标文件保持选中高亮并滚动到可视区域，绝不退化为单纯的“打开父级目录”。

### 3.2 自动窗口捕获与合并 (Auto Window-to-Tab)

- **事件监听：** 监听 `EVENT_OBJECT_CREATE` 与 `EVENT_SYSTEM_FOREGROUND`，精准捕获新实例化的 `CabinetWClass` 顶级窗口。
- **路径接管与伪装注入：**
  1. 通过 `IShellWindows` 匹配新窗口 HWND 并提取 `LocationURL` / `PIDL`。
  2. 在窗口完成首帧呈现前将其隐藏或挂起绘制，消除视觉闪烁。
  3. 向目标宿主窗口派发自动化指令，建立新 Tab 并同步载入目标路径。
  4. 回收并销毁多余的源窗口。

### 3.3 标签页去重与复用 (Tab Deduplication)

- 在用户请求打开新路径时，比对当前宿主窗口内所有已打开标签页的归一化绝对路径（Canonical Path）。
- 若目标路径已存在于某个 Tab，直接切换焦点至该 Tab，避免同路径标签页无限泛滥。
- **豁免机制说明：** 该规则仅对外部窗口合并、地址跳转生效；对于 3.6 节用户主动触发的“新建标签页继承当前目录”动作显式旁路（Bypass）。

### 3.4 标签页历史与撤销还原 (Tab History Stack)

- **关闭监听：** 监听 Tab 销毁事件或维护 Tab 列表心跳状态，当某个 Tab 路径被关闭时，推入环形历史队列（默认容量 20）。
- **快速恢复：** 提供全局接口与快捷键绑定（`ReopenClosed`，默认 `Ctrl + Shift + T`），从栈顶弹出最近一次关闭的路径并在新 Tab 中重新打开。

### 3.5 常用快捷动作支持 (Navigation Actions)

- **Duplicate Tab：** 复制当前聚焦 Tab 的路径并在同窗口建立副本。
- **Detach Tab：** 将当前激活的 Tab 剥离为独立的全新顶层窗口。
- **Escape Override：** 用户在打开文件夹时按住 `Ctrl + Shift`，强制旁路（Bypass）此模块，直接放行系统原生新窗口。

### 3.6 新建标签页继承当前目录 (Inherit Current Path on New Tab)

- **业务背景：** Windows 11 原生文件资源管理器在点击标签栏右侧“+”按钮或按下 `Ctrl + T` 时，固定打开系统启动页（“主页/主文件夹”或“此电脑”），缺少在同窗口快速克隆当前目录的能力。
- **配置开关：** 在 QuickPath 设置中心提供独立开关项（中英文多语言适配），用户可随时开启或关闭该特性。
- **触发机制：**
  1. 鼠标点击标签栏右侧“+”（新建标签页按钮）。
  2. 在已聚焦的资源管理器窗口中按下 `Ctrl + T` 快捷键。
- **核心交互时序：**
  1. **意图捕获：** 在新建 Tab 动作派发前，通过 `IShellBrowser` / `IFolderView` 捕获当前活动 Tab 的物理绝对路径 `current_path`。
  2. **新 Tab 初始化与抑制：** 放行新建 Tab 动作（或发送 `WM_COMMAND 0xA21B`）；新 Tab 创建就绪的毫秒级瞬间，挂起窗口绘制（`WM_SETREDRAW`），防止系统默认“主页”的第一帧内容闪烁。
  3. **路径重定向注入：** 对新生成的 Tab 视图调用 `IShellBrowser::BrowseObject(current_path)` 或 `IWebBrowser2::Navigate2`，将其目标路径无缝定向至 `current_path`。
  4. **恢复绘制与焦点激活：** 路径导航就绪后恢复重绘，焦点自然落入新 Tab，完成当前目录克隆。
- **与 3.3 节去重策略的优先级规则（Bypass Rule）：**
  - 用户主动点击“+”或按 `Ctrl + T` 克隆当前目录时，判定为显式并列多视图诉求，**强制旁路（Bypass）3.3 节的标签页去重逻辑**，允许同窗口内存在相同路径的多个 Tab。

---

## 4. 架构设计与技术实现方案

```
[ Windows OS / File Explorer / External Apps (IDE) ]
       │  (WinEventHook / UIAutomation / Process CommandLine)
       ▼
┌────────────────────────────────────────────────────────┐
│  QuickPath-TabModule (Rust Core)                       │
│                                                        │
│  ┌──────────────────┐    ┌───────────────────────────┐ │
│  │ ShellHookEngine  │    │ ShellComManager (STA)     │ │
│  │ (WinEventHook)   │    │ (IShellWindows/PIDL/View) │ │
│  └────────┬─────────┘    └─────────────┬─────────────┘ │
│           │                            │               │
│           └─────────────┬──────────────┘               │
│                         ▼                              │
│         ┌───────────────────────────────┐              │
│         │     TabCoordinator Engine     │              │
│         │  - 路径比对 / 选中项还原管理   │              │
│         │  - 窗口转换状态机 (FSM)       │              │
│         │  - 新建标签继承路径状态调度   │              │
│         └───────────────┬───────────────┘              │
│                         │                              │
│  ┌──────────────────────┴────────────────────────────┐ │
│  │ UIA & Shell Controller (Language-Agnostic)        │ │
│  │  - AutomationId 探测 XAML TabViewItems            │ │
│  │  - 跨进程操作/选择项重放 (SelectItem)             │ │
│  │  - 标签栏新建按钮 (AddButton) 感知与事件拦截      │ │
│  └───────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────┘
```

### 4.1 核心组件划分

1. **ShellHookEngine (原生钩子层)：**
   - 采用 `SetWinEventHook` 监听 `EVENT_OBJECT_SHOW`、`EVENT_OBJECT_DESTROY`，最小化系统全局 Hook 开销。
   - 实现低开销全局快捷键监听器，接管 `Win + E`、`Ctrl + T` 与辅助快捷动作。
2. **ShellComManager (COM 交互服务)：**
   - 运行在专属的 **STA (Single-Threaded Apartment)** 线程上，调用 `CoInitializeEx(None, COINIT_APARTMENTTHREADED)`。
   - 管理 `IShellWindows` 集合，枚举当前活跃的 Shell 窗口对象，负责 PIDL 解析、绝对物理路径提取，并通过 `IShellBrowser` 获取底层的 `IShellView`。
3. **UIA & Shell Controller (自动化与视图控制层)：**
   - Windows 11 资源管理器顶部标签栏属于 XAML 岛（类名 `ShellTabWindowClass`）。
   - 封装 `IUIAutomation` 接口，精确定位 `TabView` 与 `TabViewItem` 元素，提供跨进程 Tab 创建、切换和关闭控制。
   - 严禁依赖 UI 显示文本，统一使用 `AutomationId`（如 `AddButton`）或 `ControlType` 控件层级拓扑。
   - 封装 `IFolderView::SelectItem` / `IShellView::SelectItem` 接口，负责在目标 Tab 加载后将指定文件恢复为高亮聚焦状态。
4. **TabCoordinator (业务协调与状态机)：**
   - 基于无锁或细粒度锁结构维护内存中打开的窗口与 Tab 映射表。
   - 运行窗口捕获状态机，处理外部进程带参唤醒时的消抖与生命周期同步。
   - 调度新建标签继承路径逻辑，维护克隆动作的去重旁路标记。

### 4.2 关键 Rust Crates 选型

- `windows`：启用 `Win32_UI_WindowsAndMessaging`、`Win32_UI_Accessibility`、`Win32_System_Com`、`Win32_UI_Shell` 等特性。
- `parking_lot`：轻量级高性能锁，降低多线程状态同步开销。
- `crossbeam-channel`：跨线程任务与 STA 通信的高性能通道。
- `tracing`：纯内存结构化轻量日志，便于在测试构建中排查窗口句柄异常。

### 4.3 国际化与配置体系规格 (i18n & Settings Schema)

模块无缝融入 QuickPath 现有的国际化多语言体系，在 `locales/zh-CN.toml` 和 `locales/en-US.toml` 中配置对应键值：

#### 中文配置项 (`zh-CN.toml`)
```toml
[tab_module]
card_title = "文件资源管理器多标签页增强"
card_desc = "接管 Windows 11 原生标签页合并、去重、继承与历史回溯"
enable_tab_module = "启用多标签增强 (Tab Utility)"
enable_tab_module_desc = "自动将新建的资源管理器窗口转换为标签页合并至已有窗口"
wine_to_tab = "Win + E 自动转为新建标签页"
wine_to_tab_desc = "按下 Win+E 时在当前窗口新建标签页，避免弹出多余独立窗口"
inherit_current_path = "新建标签页打开当前目录 (Inherit Current Path)"
inherit_current_path_desc = "点击标签栏“+”号或按 Ctrl+T 时，直接克隆并打开当前所在文件夹"
tab_deduplication = "标签页智能去重 (Deduplication)"
tab_deduplication_desc = "打开已存在路径时直接切换至既有标签页，避免同路径标签页无限泛滥"
reopen_closed_tab = "恢复已关闭标签页快捷键 (Ctrl+Shift+T)"
reopen_closed_tab_desc = "按下快捷键快速重新打开最近一次关闭的文件夹标签页"
```

#### 英文配置项 (`en-US.toml`)
```toml
[tab_module]
card_title = "File Explorer Tab Utility"
card_desc = "Supercharge Windows 11 native tabs with smart merge, deduplication, and inheritance"
enable_tab_module = "Enable Tab Utility"
enable_tab_module_desc = "Automatically convert new Explorer windows into tabs in active window"
wine_to_tab = "Win + E Opens New Tab"
wine_to_tab_desc = "Open a new tab in the active window instead of launching a separate window"
inherit_current_path = "Open Current Folder on New Tab"
inherit_current_path_desc = "Clone and navigate to the current folder when clicking '+' or pressing Ctrl+T"
tab_deduplication = "Smart Tab Deduplication"
tab_deduplication_desc = "Switch to an existing tab when opening the same folder to avoid clutter"
reopen_closed_tab = "Reopen Closed Tab (Ctrl+Shift+T)"
reopen_closed_tab_desc = "Quickly restore the most recently closed tab using hotkey"
```

### 4.4 语言中立性技术规范 (Language-Agnostic Engine)

- **UIA 元素检索：** 严禁用 `CurrentName`（如“新建标签页”/“New tab”）作为选择器，强制使用固定标识符 `AutomationId == "AddButton"`，或通过 `ControlType == UIA_ButtonControlTypeId` 结合其在 `TabView` 下的子节点索引做拓扑定位。
- **系统已知路径判等：** 特殊系统目录（此电脑、主文件夹、文档、下载等）在各语言系统下本地化名称迥异，必须使用 `SHGetKnownFolderIDList` 或绝对物理路径（如通过 `IShellItem::GetDisplayName(SIGDN_FILESYSPATH)`）做标准化比对，严禁字符串展示名硬编码。

---

## 5. 关键技术难点与风险控制 (Fail-Safe)

### 5.1 难点一：外部程序 `/select` 参数捕获与高亮丢失

- **问题：** 当外部程序调用 `explorer.exe /select,"path"` 时，系统新开窗口不仅包含目录导航，还承载了选中项指令。如果单纯拦截并以常规路径打开新 Tab，将导致选中项丢失。
- **对策：**
  - 在捕获新窗口时，快速通过查询新窗口对应 `IShellBrowser` 下的当前项（或从命令行解析目标参数）。
  - 在宿主窗口中建立对应父目录 Tab 后，注册短生命周期（单次）视重视图加载事件监听，在 `FolderView` 完成文件枚举时调用 `SelectItem(item_pidl, SVSI_SELECT | SVSI_ENSUREVISIBLE)` 完成精准高亮回放。

### 5.2 难点二：跨进程死锁与 COM 阻塞

- **问题：** 当 Windows 资源管理器正处于无响应（Not Responding）或等待网络驱动器超时时，同步调用 `IShellWindows` 会导致调用方线程彻底挂起。
- **对策：**
  - 所有 COM 交互必须包含硬超时机制（统一设置上限为 150ms）。
  - 采用通道异步投递任务到 STA 工作线程，主事件捕获与系统消息循环绝不发生同步阻塞。

### 5.3 难点三：XAML 标签页结构版本差异

- **问题：** 微软在 22H2、23H2、24H2 之间持续微调资源管理器内部控件树层级与 Automation ID。
- **对策：**
  - 严禁硬编码绝对深度的 UI 树路径，采用相对模糊检索策略（按 ControlType `TabItem` 结合局部特征向上探测父级 `TabView`）。
  - 建立硬性降级方案：若 UIA 元素检索失败，立即回退至原生新窗口放行模式，绝不阻断用户正常的文件操作。

### 5.4 难点四：新窗口拦截闪烁 (Flickering)

- **问题：** 窗口创建后再销毁合并，容易在屏幕上闪烁一次黑色或白色空窗口。
- **对策：**
  - 在收到 `EVENT_OBJECT_CREATE` 的第一时间，在目标未显示前判断窗口类名并设置窗口样式或挂起重绘（`SetWindowRedraw` / `SetWindowPos` 移出可见视区），完成重定向后再行销毁。

### 5.5 难点五：中英文语言环境下的 UIA 识别失效与文本耦合陷阱

- **问题：** 许多基于 UIAutomation 的工具直接按界面可见文字（如“新建标签页”）寻找加号按钮，在英文或多语言 Windows 11 环境下会完全失灵。
- **对策：**
  - 建立双通道探测机制：优先采用 `AutomationId`（`AddButton`）探测；若未命中则通过识别父容器 `TabView` 并取其最末端的功能性 Button 控件；或者直接通过底层消息 `WM_COMMAND 0xA21B` 触发原生新建，彻底剥离对可见文本的依赖。

### 5.6 难点六：新建标签页继承路径时的时序竞争与重绘闪烁

- **问题：** 点击“+”号后，系统原生会异步初始化“主页”或“此电脑”；若重定向指令下发过慢，用户会明显看到界面先显示“主页”再跳变到当前目录。
- **对策：**
  - **路径预先捕获：** 在加号点击前（或按下 `Ctrl+T` 按键触发的纳秒级时间内）立即捕获当前 Tab 路径并暂存。
  - **绘制锁定时序：** 在新 Tab 句柄挂载到 `CabinetWClass` 的首个时钟周期，调用 `SendMessageW(hwnd, WM_SETREDRAW, FALSE, 0)` 冻结重绘，直接下发 `BrowseObject` 指令，待导航完成确认后再解冻重绘（`WM_SETREDRAW, TRUE`），实现肉眼无感切换。

---

## 6. 测试与验证策略

遵照项目工程规范，测试代码与临时排查工具统一收敛在 `test/` 目录下：

```
test/
├── tab_hook_probe/           # 独立的 WinEvent 事件捕获控制台工具，用于实时 dump 系统事件
├── select_item_test/         # 模拟外部 IDE 触发 /select 行为并验证文件高亮还原的测试工具
├── uia_inspector/            # 遍历当前系统 Explorer 窗口内部 UIA 结构的诊断工具（验证 AutomationId 与层级）
├── com_shell_test/           # IShellWindows 枚举与 PIDL 提取的独立压力测试
├── new_tab_inherit_test/     # 针对新建标签页继承当前目录的重定向延迟与防闪烁测试工具
└── i18n_compatibility_test/  # 中文与英文语言环境下的自动化探针兼容性验证脚本
```

### 验证指标表

- **内存占用测试：** 工具在后台平稳待机 4 小时，物理内存常驻工作集不得超过 **12MB**。
- **CPU 占用测试：** 系统空闲时，进程 CPU 占用率稳定为 **0.00%**。
- **并发压力测试：** 连续快速打开 20 个不同路径文件夹，窗口合并成功率需在 **95%** 以上，无悬挂的孤儿资源管理器进程残留。
- **场景保真度测试：** 在 VSCode、IDEA 与 Chrome 下载列表中点击“在文件夹中显示”，文件高亮选中与视区居中成功率需达到 **100%**。
- **新建标签继承延迟：** 点击加号按钮至当前目录重定向呈现完毕，全流程耗时不得超过 **35ms**，无肉眼可见的“主页”二次闪烁。
- **国际化环境兼容性：** 在纯英文 Windows 11 (en-US) 与纯中文 Windows 11 (zh-CN) 镜像下，探针识别率与各功能执行成功率均达到 **100%**。

---

## 7. 阶段实施计划 (Milestones)

- **Stage 1 (基础设施与原型验证)：**
  - 搭建 Rust 独立子包架构，配置 Windows API 依赖。
  - 在 `test/com_shell_test` 中跑通基于 `windows` crate 的 `IShellWindows` 枚举与 PIDL 路径解析。
  - 在 `test/uia_inspector` 中提取 Win11 标签栏加号按钮在不同语言下的固定 `AutomationId` 与控件树结构。
- **Stage 2 (窗口接管与场景覆盖)：**
  - 实现基于 `SetWinEventHook` 的后台守护监听器。
  - 完成 `Win + E` 的宿主状态检测与分支动作分流。
  - 在 `test/select_item_test` 中攻克从外部调用中提取目标项并在新 Tab 中重放 `SelectItem` 高亮逻辑。
- **Stage 3 (新建标签继承当前目录与防闪烁)：**
  - 在 `test/new_tab_inherit_test` 中实现加号点击感知、路径预存与新 Tab 毫秒级重定向。
  - 攻克 `WM_SETREDRAW` 挂起重绘机制，消除新 Tab 重定向闪烁。
  - 实现与 3.3 节去重策略的旁路互斥逻辑。
- **Stage 4 (Tab 注入与 UIA 驱动)：**
  - 编写 `IUIAutomation` 交互驱动，跑通在宿主窗口自动增加 Tab、切换 Tab 的全流程。
  - 消除窗口合并过程中的黑白屏闪烁。
- **Stage 5 (国际化适配与 QuickPath 模块化整合)：**
  - 将所有设置项同步至 `locales/zh-CN.toml` 和 `locales/en-US.toml`。
  - 完善 Tab 历史栈、重复路径过滤及 `Ctrl+Shift` 旁路逻辑。
  - 将核心驱动导出为 QuickPath 主进程可调用的静态/动态组件，提供标准启停与配置下发 API。