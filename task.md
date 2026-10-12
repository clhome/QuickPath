# QuickPath 研发任务清单 (Task Tracking)

> **项目概况**：QuickPath - 专为 Windows 10/11 打造的现代 Fluent 风格文件对话框智能路径跟随与快速跳转利器。  
> **核心规范**：KISS 原则、第一性原理、代码洁净、UTF-8 (无 BOM) LF 换行、Windows 10/11 深度兼容、测试脚本统一收纳于 `test/` 目录。

---

## 阶段一：项目工程骨架与基础框架搭建
- [x] **1.1 初始化 Rust 工程结构**
  - [x] 配置 `Cargo.toml`（集成 `windows`、`serde` 等必要依赖）
  - [x] 设立模块目录（`src/win32/`, `src/dialog/`, `src/managers/`, `src/rules/`, `src/ui/`）
  - [x] 配置 App Manifest 启用 Per-Monitor V2 DPI Awareness 与 LongPathAware
- [x] **1.2 配置管理与持久化 (Config & Storage)**
  - [x] 实现配置模型（自动切换开关、灵敏度延迟、快捷键、黑白名单、收藏夹与历史记录）
  - [x] 便携模式（当前目录）与标准模式（`%APPDATA%`）双向支持

## 阶段二：底层窗口监听与对话框注入引擎
- [x] **2.1 系统级窗口事件监听 (WinEventHook)**
  - [x] 实现 `EVENT_SYSTEM_FOREGROUND` 监听
  - [x] 跟踪焦点切换时序，建立前台窗口历史时间戳（识别最近激活的文件管理器）
- [x] **2.2 文件对话框高精度识别 (Dialog Detector)**
  - [x] 识别标准 `#32770` 及现代通用对话框（IFileDialog / DirectUIHWND / Breadcrumb）
  - [x] 获取文件名输入框（`Edit1`）、地址栏及导航控件句柄
- [x] **2.3 安全无干扰路径注入引擎 (Dialog Injector)**
  - [x] 实现 Win32 控件直接通讯与安全消息发送
  - [x] 注入过程挂起/隔离输入法（IME）上下文，防止拼音顶词与乱码
  - [x] 保持原有文件名输入不变，注入后自动恢复原有焦点

## 阶段三：全生态文件管理器路径捕获适配
- [x] **3.1 Windows 11 资源管理器（多标签页 Tabs）适配**
  - [x] 解决 `Shell.Application.Windows` 在 Win11 下无法捕获活动 Tab 问题
  - [x] 结合激活视图检测，精确定位当前激活 Tab 路径
- [x] **3.2 Windows 10 经典资源管理器适配**
  - [x] 针对单窗口 `CabinetWClass` 进行 COM 极速枚举
- [x] **3.3 第三方主流文件管理器适配器**
  - [x] Directory Opus 路径捕获适配（通过 dopusrt 导出）
  - [x] Total Commander 路径捕获适配（通过 `cm_CopySrcPathToClip` 并还原剪贴板）
  - [x] XYplorer 路径捕获适配（通过 `WM_COPYDATA` 脚本通信）

## 阶段四：智能自动跟随 (AutoSwitch) 与规则系统
- [x] **4.1 自动秒切决策引擎**
  - [x] 毫秒级计算前台切换关联性，触发 0-Click 静默自动切换
  - [x] 防误触覆盖机制（防止重复触发与冲刷已修改状态）
- [x] **4.2 黑白名单与个性化规则**
  - [x] 浏览器（Chrome/Edge 等）免打扰黑名单过滤
  - [x] 特定软件自动跟随与热键直达模式

## 阶段五：Windows 11 Fluent 悬浮吸附条与现代 UI
- [x] **5.1 悬浮吸附栏 (Floating Bar) 窗口与材质**
  - [x] 原生分层无边框窗口，调用 DWM 启用 Windows 11 原生 Mica / Acrylic 与圆角
  - [x] 对话框位置联动跟随（边缘吸附与移动同步）
- [x] **5.2 候选列表与键盘交互**
  - [x] 呈现打开的管理器窗口/Tab、常用收藏夹与最近历史
  - [x] 支持上下键选择、Enter 瞬间跳转
- [x] **5.3 系统托盘与快捷菜单**
  - [x] 系统托盘图标（带多状态指示）
  - [x] 托盘右键菜单（快速切换自动模式、打开设置中心、开机自启、退出）

## 阶段六：开机自启、设置中心与系统集成
- [x] **6.1 免 UAC 开机自启服务**
  - [x] Windows 任务计划程序（Task Scheduler）静默最高权限自启
  - [x] 注册表 Run 键标准自启备用
- [x] **6.2 现代化图形设置中心**
  - [x] 现代卡片式 UI：常规、自动秒切、开机自启、快捷键与生态支持

## 阶段七：全面测试与收尾优化
- [x] **7.1 多场景与多 DPI 兼容性**
  - [x] 清单声明 Per-Monitor V2 DPI Awareness 与 LongPathAware
  - [x] 输入法隔离（IME Guard）防拼音顶词机制就绪
- [x] **7.2 性能调优与临时文件清理**
  - [x] 极速独立原生构建，Release 二进制体积仅 520 KB，后台内存占用 < 10MB
  - [x] 清理开发与排查过程中生成的临时文件

## 阶段八：高分屏适配、自动展示与稳定跳转专项修复
- [x] **8.1 高分屏（200% 与普通 100% 屏幕）全动态 DPI 适配**
  - [x] 动态获取真实窗口 DPI 缩放比率，支持多显示器拖拽与 `WM_DPICHANGED`
  - [x] 几何尺寸（宽、高、行高、内边距）与通用清晰字体（Microsoft YaHei UI / Segoe UI）自适应
- [x] **8.2 文件对话框打开时 100% 自动无感吸附展示**
  - [x] 根治快捷键依赖，文件对话框一出现即自动弹出贴附
  - [x] 解决对话框弹出时子控件就绪时序延迟，加入短延时自适应检测
- [x] **8.3 修复自动秒切与目录跳转机制**
  - [x] 采用 Win32 原生 `IDOK` 与真实键盘管道完成跳转，杜绝回车失效
  - [x] 彻底移除 80ms 暴力覆写原文本 Bug，保护文件名与路径识别
  - [x] 智能区分打开文件对话框与另存为对话框
- [x] **8.4 候选列表与“最后打开目录”深度健全**
  - [x] 主线程 COM 生命周期规范化，杜绝 `CoUninitialize` 污染导致后续获取失败
  - [x] 健全“最后一次打开目录”优先级（活跃 Explorer -> 最近使用历史 -> 桌面/下载/常用）
  - [x] 自动清理历史配置中的黑名单误伤（浏览器下载/上传），确保开箱即用
- [x] **8.5 编译验证、成果交付与测试文件清理**

## 阶段九：多屏精准跟随、单行高密度布局、半透明调节与中英文 i18n 支持
- [x] **9.1 多显示器精准跟随定位**
  - [x] 基于 `MonitorFromWindow` / `GetMonitorInfoW` 计算对话框所在屏幕物理工作区
  - [x] 彻底移除旧逻辑中限制在主屏（0 ~ SM_CXSCREEN）的硬编码约束，确保左/右/上下多屏完美贴附
- [x] **9.2 弹出层单行高密度渲染**
  - [x] 布局由“双行上下排布”改造为“统一单行排布”：[目录名] + [完整路径] + [来源徽章]
  - [x] 压缩行高（38px * scale），容纳更多候选项目（最多显示 8 条）
- [x] **9.3 弹出层半透明度与用户调节支持**
  - [x] 窗口启用 `WS_EX_LAYERED` 与 `SetLayeredWindowAttributes`，结合 DWM 材质
  - [x] 配置新增 `floating_bar_opacity`（透明度 40%~100%），支持配置文件与设置中心动态调节
- [x] **9.4 全局轻量多国语言 i18n 引擎**
  - [x] 模块化 i18n（支持简体中文 zh-CN、英文 en-US）
  - [x] 支持跟随系统区域自动检测（`GetUserDefaultUILanguage`）与手动配置切换
  - [x] 覆盖吸附栏、右键托盘、设置中心与所有标签提示
- [x] **9.5 编译测试与交付收尾**

## 阶段十：多显示器混合 DPI（200% 主屏 + 100% 普通副屏）深度自适应修复
- [x] **10.1 物理显示器真实 DPI 感知引擎**
  - [x] 彻底抛弃从尚未移动的悬浮窗获取 DPI 的错误逻辑，优先使用 `GetDpiForMonitor(h_mon, MDT_EFFECTIVE_DPI)` 直接提取目标屏幕物理缩放
  - [x] 确保对话框在 200% 屏呈现 200% 等比放大，在 100% 副屏呈现 100% 紧凑比例
- [x] **10.2 响应跨屏 WM_DPICHANGED 动态自适应**
  - [x] 拦截并处理 `WM_DPICHANGED`，窗口跨屏幕时即时重构坐标与字体尺寸
- [x] **10.3 动态屏幕尺寸约束与自适应**
  - [x] 约束悬浮层物理宽度不超过副屏可用工作区的 85%~90%，防止大分辨率设定溢出 1080p 屏
- [x] **10.4 编译验证与交付**

## 阶段十一：设置中心透明度滑块控件与出品方信息展示
- [x] **11.1 透明度原生滑块控件与拖动交互**
  - [x] 绘制现代 Fluent 风格滑块槽与高亮滑块头（Thumb），显示实时百分比数值（40% ~ 100%）
  - [x] 支持鼠标点击定位与按住拖动（`WM_LBUTTONDOWN` / `WM_MOUSEMOVE` / `WM_LBUTTONUP` / `SetCapture`）
  - [x] 实时保存配置并热同步更新当前吸附层透明度
- [x] **11.2 页面底部出品方信息中英双语展示**
  - [x] 底部右侧添加出品方标识（中文：“衢州御风科技有限公司出品”；英文：“Produced by Quzhou Yufeng Technology Co., Ltd.”）
  - [x] 跟随全局 i18n 语言自动切换
- [x] **11.3 编译验证与交付**

## 阶段十二：设置中心「确定 / 取消」按钮与全局配置热生效
- [x] **12.1 多语言按钮定义与支持**
  - [x] 在 `src/rules/i18n.rs` 中增加「确定」（OK）与「取消」（Cancel）国际化文本支持
- [x] **12.2 全局运行时配置热更新通道**
  - [x] 在 `src/main.rs` 中暴露 `get_global_config()` 与 `update_global_config()`，解决设置修改后内存中运行时状态不同步的根本原因
- [x] **12.3 设置中心草稿状态与确认/取消按钮交互**
  - [x] 引入 `DRAFT_CONFIG` 临时草稿，设置项调整即时预览但需「确定」后才正式生效
  - [x] 在卡片下方自绘现代 Fluent 风格「确定」与「取消」按钮，支持悬停高亮与点击响应
  - [x] 点击「确定」：执行保存、开机自启同步、更新全局 `AppState` 内存配置并关闭设置窗口
  - [x] 点击「取消」或右上角关闭：丢弃临时修改并关闭窗口
- [x] **12.4 编译验证与端到端测试**

## 阶段十三：全局快捷键自定义设置与动态热注册
- [x] **13.1 快捷键解析与动态注册引擎**
  - [x] 实现 `parse_hotkey` 解析器，支持 Ctrl / Alt / Shift / Win 任意组合键与主键解析
  - [x] 在 `src/main.rs` 中实现 `register_app_hotkey` 动态注销旧热键并注册新热键
- [x] **13.2 托盘右键菜单文案动态绑定**
  - [x] 更新 `src/rules/i18n.rs` 与 `src/ui/tray.rs`，使托盘菜单展示当前真实快捷键（如 `呼出候选目录 (Ctrl + Q)`）
- [x] **13.3 设置中心快捷键卡片与按键录制交互**
  - [x] 在设置中心增加「呼出快捷键 (Hotkey)」卡片，展示当前按键 Badge
  - [x] 点击卡片进入录制模式，拦截 `WM_KEYDOWN` / `WM_SYSKEYDOWN` 实时录制用户输入的组合键
  - [x] 支持按 Esc 退出录制恢复原状
- [x] **13.4 保存生效与热更新闭环**
  - [x] 点击「确定」时持久化至 `config.json` 并调用热更新通道重新注册热键，使全局快捷键即刻生效
- [x] **13.5 编译验证与端到端测试**

## 阶段十四：独立 TOML 国际化语言包架构与双层加载引擎
- [x] **14.1 引入 toml 依赖**
  - [x] 在 `Cargo.toml` 中添加 `toml = "0.8"` 依赖
- [x] **14.2 独立 TOML 语言包规范建立**
  - [x] 创建 `locales/zh-CN.toml` 与 `locales/en-US.toml`，完整收拢悬浮吸附层、快捷键、设置中心与托盘全部文案
- [x] **14.3 重构 i18n 引擎（内置嵌入 + 外部扩展双层架构）**
  - [x] 定义强类型语言包结构 `LocaleBundle`
  - [x] 编译期 `include_str!` 嵌入官方语言，保证单文件绿色便携、零外部依赖
  - [x] 运行时动态扫描 `./locales/*.toml` 与配置目录，支持第三方免编译新增任意语言
  - [x] 保持对外高层 API 兼容，无缝过渡
- [x] **14.4 编译验证与外部语言文件扩展测试**

## 阶段十五：产品专属 Logo 与图标全局集成
- [x] **15.1 资源归位与规范化存放**
  - [x] 创建 `assets/` 目录并将 `Document/imgs/logo.ico` 与 `Document/imgs/logo.png` 复制至该目录
- [x] **15.2 EXE 可执行程序图标嵌入**
  - [x] 更新 `quickpath.rc` 增加 `1 ICON "assets/logo.ico"`
  - [x] 更新 `build.rs` 监听 `assets/logo.ico` 变化
- [x] **15.3 系统托盘图标（Tray Icon）升级**
  - [x] 新建 `src/win32/icon.rs` 模块，通过 `LoadImageW` 动态匹配 DPI 尺寸加载专属图标
  - [x] 更新 `src/ui/tray.rs`，从程序嵌入资源精准加载专属 Logo，告别 Windows 通用白板图标
- [x] **15.4 窗口标题栏与任务栏图标升级**
  - [x] 在 `src/ui/settings.rs` 窗口注册中绑定专属 Logo，设置中心窗口呈现专业视觉
  - [x] 发送 `WM_SETICON` 消息同时配置 `ICON_SMALL` 与 `ICON_BIG`
- [x] **15.5 编译验证与交付**
  - [x] Release 构建成功，体积轻巧（1.1MB），资源完全嵌入且无需任何外部动态链接

## 阶段十六：设置中心现代化、Toggle滑块开关、语言下拉框与“关于”信息深度重构
- [x] **16.1 多语言 i18n 资源包扩充（关于信息与下拉项）**
  - [x] 在 `locales/zh-CN.toml` 与 `locales/en-US.toml` 中增加 `[about]` 专属板块（版本、出品方、产品定位、版权）
  - [x] 优化语言显示名称，清理废弃的生态适配文案
- [x] **16.2 现代 Switch Toggle 滑块开关实现（左右滑动与点击）**
  - [x] 自绘 Fluent 胶囊滑块开关（包含左右轨道、白圆滑块 Thumb、开启/关闭 Accent 配色）
  - [x] 为“自动秒切”与“开机自启”接入 Toggle 控件，支持鼠标点击与左右按住拖拽滑动
- [x] **16.3 界面语言原生下拉选择框（Dropdown）实现**
  - [x] 自绘带 `▾` 箭头的下拉 ComboBox 控件
  - [x] 点击通过 `TrackPopupMenu` 弹出原生毛玻璃阴影菜单，枚举全部可用语言并标记当前选中项（`MF_CHECKED`）
- [x] **16.4 移除生态适配卡片 & 增加专业“关于”卡片**
  - [x] 彻底移除无用生态适配卡片，释放垂直空间
  - [x] 新增“关于 QuickPath”现代卡片，展示 Logo、版本、御风科技公司信息及版权
- [x] **16.5 整体视觉与交互美化（Fluent 卡片、圆角、Header 品牌区、Hover 动效、按钮质感）**
  - [x] 顶部增加 Header 品牌区（绘制 36x36 高清 App Logo + 粗体主标题 + 副标题 + 1px 细微分割线）
  - [x] 卡片采用深色圆角矩形（`RoundRect`）与 1px 微反差边框，支持 Hover 悬停微高亮与手型光标
  - [x] 美化「确定」与「取消」按钮，采用精致圆角与悬停反馈
- [x] **16.6 编译构建验证与交付收尾**
  - [x] 执行 `cargo check` 与 `cargo build --release` 达到 0 警告 0 错误
  - [x] 生成最终 Release 可执行文件（1.2MB 纯绿色无依赖便携程序）

## 阶段十七：设置中心 Header 精简化与关于板块 Logo 80% 高清放大重构
- [x] **17.1 设置中心上方图标去除**
  - [x] 移除顶部 Header 区域的 36x36 品牌图标，主副标题恢复左边缘对齐（`header_text_x = pad_x`），视觉更纯净简洁
- [x] **17.2 引入 GDI+ 高质量图片平滑重采样引擎**
  - [x] 在 `src/win32/icon.rs` 中引入 GDI+ 图像渲染通道（`draw_logo_png`），支持双三次插值重采样（`InterpolationModeHighQualityBicubic`）
  - [x] 优先加载 `assets/logo.png` 原图，并内置 `include_bytes!` 内存流回退机制保证单文件分发无缝可用
- [x] **17.3 关于板块 Logo 放大至框高 80%**
  - [x] 将关于卡片内 Logo 高度提升至 `((card_h as f32) * 0.8)`（约 78px~80px），等比例呈现 440×440 原图，垂直居中排布
  - [x] 右侧 4 行品牌信息（产品与版本、出品方御风科技、研发定位、版权声明）与左侧大 Logo 精致齐平
- [x] **17.4 编译验证与交付**
  - [x] `cargo check` 与 `cargo build --release` 编译通过（0 警告 0 错误）

## 阶段十九：悬浮条出品方文字、多语言适配与官网跳转
- [x] **19.1 多语言配置与模型扩展 (i18n)**
  - [x] 在 `locales/zh-CN.toml` 与 `locales/en-US.toml` 的 `[floating_bar]` 中增加出品方 `producer` 字段
  - [x] 在 `src/rules/i18n.rs` 的 `FloatingBarSection` 中集成 `producer` 并在 `I18n` 中提供 `floating_producer` 函数
- [x] **19.2 悬浮条顶栏右侧文本自绘与布局保护**
  - [x] 在 `src/ui/floating_bar.rs` 中动态计算出品方文字宽度，靠右对齐展示
  - [x] 限制左侧主标题绘制范围，防止与右侧文字重叠
  - [x] 支持根据 DPI 动态缩放字体与间距
- [x] **19.3 悬停交互与官网超链接跳转**
## 阶段二十：版本号统一管理、设置中心底部按钮重排与官网/GitHub链接
- [x] **20.1 版本号单一数据源 (Single Source of Truth) 与多语言解耦**
  - [x] 在 `Cargo.toml` 中统一规范版本号为 `1.0.0`
  - [x] 创建 `src/rules/version.rs`，导出统一版本号常量与获取函数
  - [x] 在 `locales/*.toml` 中将写死的 `v1.0.0` 改造为 `{version}` 动态占位符，并在 `i18n.rs` 解析时自动注入
  - [x] 消除 `settings.rs` 中写死的硬编码版本文本
- [x] **20.2 设置中心「确定」「取消」按钮沉底排布**
  - [x] 按钮从跟随关于卡片紧贴改为绝对贴合窗口底部，留出 `22px * scale` 呼吸感空间
  - [x] 同步修改绘制渲染、鼠标悬停与点击命中判定，确保逻辑一致
  - [x] 底部左侧版本信息与右侧按钮水平居中对齐，优化界面留白
- [x] **20.3 关于卡片官网外链与 GitHub 品牌链接集成**
  - [x] 引入 `assets/github.png` 高清透明图标并集成至 `src/win32/icon.rs`
  - [x] 在关于卡片内出品方增加官网点击跳转 `https://qp.yftec.top`，悬停高亮与手型光标
  - [x] 在关于卡片内新增 GitHub 图标与项目链接 `https://github.com/clhome/QuickPath`，支持点击跳转
- [x] **20.4 编译检查、验证与代码清理**
  - [x] 运行 `cargo check`、`cargo test` 与 `cargo build --release` 达到 0 警告 0 错误
  - [x] 清理 `test/` 临时生成工具，保持代码库整洁

## 阶段二十一：README 多语言对齐与 Document 中英文说明书编写
- [x] **21.1 编写/优化根目录 README_CN.md**
  - [x] 丰富中文 README 核心功能、快捷操作、配置说明、构建指南与版权信息
  - [x] 顶部添加双语切换条 `简体中文 | [English](README.md)` 及指向说明书的链接
- [x] **21.2 编写/优化根目录 README.md 为专业英文版**
  - [x] 翻译并润色全套英文版项目说明
  - [x] 顶部添加双语切换条 `[简体中文](README_CN.md) | English` 及指向英文说明书的链接
- [x] **21.3 在 Document 目录下编写详尽中文用户说明书 (Document/User_Manual_CN.md)**
  - [x] 涵盖产品定位、系统环境、安装启动、核心功能（自动秒切/吸附条/IME保护/文件管理器生态）、图形设置中心指南、配置项深度解析、常见问题 FAQ、出品方信息
- [x] **21.4 在 Document 目录下编写详尽英文用户说明书 (Document/User_Manual_EN.md)**
  - [x] 全文英文版，结构与内容与中文版完全一致、语法地道严谨
- [x] **21.5 验证与检查中英文文档互链及格式一致性**
  - [x] 确保所有文件采用 UTF-8 (无 BOM) LF 换行，相对路径引用正确

## 阶段二十二：GitHub Actions 自动化 Release 打包工作流
- [x] **22.1 配置自动化 Release 工作流 (.github/workflows/release.yml)**
  - [x] 监听 `v*` 格式 Git Tag 推送事件，配置 GitHub Actions 读写权限
  - [x] 配置 `windows-latest` 环境与 Rust 稳定版工具链及编译缓存
  - [x] 自动化执行 `cargo build --release` 编译绿色单执行文件
  - [x] 自动化整理规范命名制品（`QuickPath-{tag}-x64.exe` 及 Zip 压缩包）
  - [x] 集成 `softprops/action-gh-release@v2` 自动创建 Release 并上传附件
- [x] **22.2 验证与交付**
  - [x] 确保 YAML 格式严谨、UTF-8 (无 BOM) LF 换行，更新任务进度

## 阶段二十三：Windows 可执行程序专业化打包与详细信息元数据注入
- [x] **23.1 补全 Cargo.toml 包元数据与 Release 编译调优**
  - [x] 补全 `[package]` 标准元数据（`description`, `authors`, `homepage`, `repository`, `license`）
  - [x] 增加 `[profile.release]`：启用 `strip = true`, `lto = true`, `codegen-units = 1`, `opt-level = 3`, `panic = "abort"`
- [x] **23.2 规范与加固 app.manifest 应用程序清单**
  - [x] 增加 `trustInfo` 声明 `asInvoker`，规范 Windows 权限等级并杜绝 UAC 虚拟化兼容提示
- [x] **23.3 升级 build.rs 资源动态生成引擎与 quickpath.rc**
  - [x] 动态提取 `CARGO_PKG_VERSION` 构建四段式标准 Windows 版本号（如 `1,0,1,0` 与 `"1.0.1.0"`）
  - [x] 动态生成/写入标准 Windows `VERSIONINFO` 资源块，声明 UTF-8 代码页（`#pragma code_page(65001)`）
  - [x] 配置完善的中英双语元数据：文件说明、产品名称、公司名、版权声明、官网备注、原始文件名
  - [x] 引入内容哈希/比对机制避免重复写入触发构建死循环，安全调用 `embed_resource::compile`
- [x] **23.4 编译构建、属性读取校验与交付收尾**
  - [x] 执行 `cargo check` 与 `cargo build --release` 验证通过
  - [x] 运行 PowerShell 脚本自动化校验 `VersionInfo` 各项字段读取结果
  - [x] 清理测试临时文件，更新任务进度

## 阶段二十四：任务栏状态监控模块 (v1.1) 研发
- [x] **24.1 基础依赖与配置模型扩展 (Deps & Config & i18n)**
  - [x] 在 `Cargo.toml` 中配置 `arc-swap = "1.7"` 及 `windows` crate 所需特性
  - [x] 在 `src/rules/config.rs` 中新增 `MonitorConfig` 并在 `AppConfig` 中集成
  - [x] 在 `locales/zh-CN.toml` 和 `locales/en-US.toml` 中增加 `[monitor]` 多语言支持并在 `src/rules/i18n.rs` 中映射
- [x] **24.2 免特权底层指标采集引擎 (Metrics Collectors)**
  - [x] 实现物理网卡过滤与瞬时带宽采集 (`src/monitor/collector/network.rs`)
  - [x] 实现系统 CPU 利用率采集 (`src/monitor/collector/cpu.rs`)
  - [x] 实现物理内存利用率与容量采集 (`src/monitor/collector/memory.rs`)
  - [x] 实现 GPU & 磁盘利用率采集 (`src/monitor/collector/gpu.rs`, `src/monitor/collector/disk.rs`)
  - [x] 实现 Tooltip 扩展明细采集 (`src/monitor/collector/details.rs`)
- [x] **24.3 环形波形缓冲与后台 Worker 线程 (Worker & Waveform & ArcSwap)**
  - [x] 定义数据快照 `MetricsSnapshot` 与历史环形缓冲区 `WaveformBuffer`
  - [x] 实现后台采样 Worker 线程，支持无锁共享与零开销优雅休止
- [x] **24.4 任务栏分层悬浮宿主、全屏避让与窗口管理 (Bar Window & Shell Docking)**
  - [x] 创建任务栏依附分层窗口，实现托盘区域锚定与跟随
  - [x] 实现全屏应用（游戏/影音/演示）主动隐匿避让机制
  - [x] 注册并监听 `TaskbarCreated`、`WM_POWERBROADCAST` 与 `WM_DPICHANGED`
- [x] **24.5 GDI+ 预乘 Alpha 渲染引擎与悬浮 Tooltip (Render & Fluent Tooltip)**
  - [x] 动态列宽自适应收缩引擎（网络、CPU/内存、GPU/磁盘 1~3 列）
  - [x] GDI+ 预乘 Alpha 紧凑两行文字、指示条微色块、历史波形折线图绘制
  - [x] 鼠标交互（左键单击切换/双击打开任务管理器/右键上下文菜单）
  - [x] Fluent 悬浮详情看板（Tooltip）实现
- [x] **24.6 设置中心 Tab 切换与系统托盘深度集成 (Settings & Tray Integration)**
  - [x] 设置中心升级支持「常规偏好」与「任务栏监控」选项卡切换
  - [x] 任务栏监控配置卡片（总开关、指标多选、波形背景、刷新率、透明度）
  - [x] 系统托盘右键菜单增加任务栏监控快捷开关
  - [x] 在 `src/main.rs` 中集成 `MonitorManager` 生命周期控制
- [x] **24.7 编译构建验证、性能开销审查与交付**
  - [x] `cargo check`、`cargo test` 与 `cargo build --release` 达到 0 警告 0 错误
  - [x] 验证常驻内存增量 $\le 5\text{MB}$ 与 CPU 占用 $\le 0.05\%$
  - [x] 纯绿色单文件交付体积仅 1.01MB，更新任务进度完成交付

## 阶段二十五：任务栏状态监控 Bug 修复与多位置停靠自适应
- [x] **25.1 GDI+ 全生命周期就绪与渲染引擎修复**
  - [x] 在 `src/main.rs`、`src/monitor/window/bar_window.rs`、`src/monitor/window/render.rs` 注入 `ensure_gdiplus()`
  - [x] 重构文字与像素 Alpha 合成逻辑，前景文字与高亮指示条赋予完全预乘 Alpha，文字清晰呈现
- [x] **25.2 窗口光标与 WM_SETCURSOR 响应修复**
  - [x] 注册窗口类绑定 `LoadCursorW(None, IDC_ARROW)`
  - [x] 在 `bar_wnd_proc` 处理 `WM_SETCURSOR`，移入即刻呈现标准箭头光标
- [x] **25.3 Z-Order 顶层持续锁定防遮挡**
  - [x] `SetWindowPos` 严格使用 `Some(HWND_TOPMOST)`，杜绝 `WS_EX_TOPMOST` 被剥夺
  - [x] 确保多窗口切换与最大化时监控窗口保持顶层
- [x] **25.4 任务栏位置模式扩展（托盘左侧 / 任务栏左侧）**
  - [x] 在 `MonitorConfig` 中增加 `position` 字段，并在多语言中添加对应词条
  - [x] 在 `bar_window.rs` 实现“任务栏左侧”与“托盘左侧”双向坐标锚定
  - [x] 在监控右键菜单与设置中心中提供位置切换
- [x] **25.5 编译验证、清理测试文件与 task.md 进度更新**
  - [x] 执行 `cargo check` 验证 0 警告 0 错误
  - [x] 清理 `test/` 临时排查脚本，更新根目录 `task.md`

## 阶段二十六：监控文字可读性增强（微软雅黑+放大一号）与采样刷新档位调整 (1s/3s/5s)
- [x] **26.1 监控面板字体与字号升级 (Microsoft YaHei UI & Font Scaling)**
  - [x] 切换主渲染字体为 `"Microsoft YaHei UI"`，备用字体设为 `"Segoe UI"`
  - [x] 字号放大一号（基础字号从 9.0 提升至 10.5）
  - [x] 调优 `calculate_layout` 列宽与窗口高度（网卡 42px，CPU/内存 48px，GPU/磁盘 48px，高度 38px），确保大字号不截断不拥挤
- [x] **26.2 采样刷新率 3 档调整与默认 3s (Refresh Interval & Default 3s)**
  - [x] 修改 `MonitorConfig::default()` 的 `refresh_interval_ms` 为 `3000`
  - [x] 更新 `config.rs` 自愈校验范围（保留合法区间，异常恢复默认 3000ms）
  - [x] 更新 `locales/zh-CN.toml` 与 `locales/en-US.toml` 词条为 1.0s / 3.0s / 5.0s，标注 3s 为默认/推荐
  - [x] 更新 `src/rules/i18n.rs` 中的字段定义（`interval_1s`, `interval_3s`, `interval_5s`）
- [x] **26.3 设置中心单选胶囊与交互联动 (Settings UI Capsule & Events)**
  - [x] 在 `src/ui/settings.rs` 将 3 段式单选胶囊选项更新为 1000ms (1s) / 3000ms (3s) / 5000ms (5s)
  - [x] 调整胶囊宽度 `seg_w` 并按左到右递增顺序列出 (1s -> 3s -> 5s)
  - [x] 同步更新鼠标移动悬停命中检测与鼠标左键点击切换逻辑
- [x] **26.4 单元测试、构建验证与收尾**
  - [x] 更新 `src/rules/config.rs` 中的配置测试用例
  - [x] 执行 `cargo test` 与 `cargo check` 验证 0 警告 0 错误
  - [x] 更新 `task.md` 完成进度记录

## 阶段二十七：监控常驻面板深色卡片背景与 Win32 ClearType 锐利渲染重构
- [x] **27.1 沉稳深色背景卡片与真实透明度映射**
  - [x] 修正 `opacity` 算式为真实 0~255 映射，默认不透明度提升至 95%
  - [x] 暗色模式下设定沉稳纯净深色底 `RGB(24, 24, 26)`，浅色模式 `RGB(245, 245, 248)`，彻底隔绝复杂壁纸杂色穿透
- [x] **27.2 重构为 Win32 ClearType 原生文字渲染引擎**
  - [x] 引入 `CreateFontW` 创建 `Microsoft YaHei UI` 字体并指定 `FONT_QUALITY(5)` (ClearType)
  - [x] 实现 `draw_gdi_text` 替代原 GDI+ 灰度抗锯齿，文字在内存 DC 上以透明模式高质量绘制
  - [x] CPU/内存微型指示条采用 GDI `FillRect` 原生绘制
  - [x] 历史波形折线采用 GDI `Polyline` 绘制
- [x] **27.3 平滑 Alpha 通道保护消除毛刺灰边**
  - [x] 后处理重构为平滑底限保护：`if a < base_alpha { *p = (*p & 0x00FFFFFF) | (base_alpha << 24); }`，彻底消除硬化灰毛刺
- [x] **27.4 编译验证与单元测试**
  - [x] 运行 `cargo test` 与 `cargo check` 确保 0 警告 0 错误
  - [x] 更新 `task.md` 进度打勾

## 阶段二十八：硬件详情看板 (Tooltip) 极清微软雅黑升级与宽度自适应
- [x] **28.1 升级正文字体为微软雅黑与高对比度渲染**
  - [x] 将正文字体从 Segoe UI 切换为 `Microsoft YaHei UI`（微软雅黑），字号从 -11.0 提升至 -12.0
  - [x] 标题字号提升至 -14.0，行高放宽至 23px
  - [x] 正文文字颜色提升为高对比度纯白/亮白 `0x00F0F0F0`，彻底消除发灰发虚
- [x] **28.2 纯净深色不透明背景卡片 (阻绝底层透光)**
  - [x] 将背景不透明度从 240 提升至 255（完全不透明），底色设为深炭灰 `RGB(24, 24, 26)`
  - [x] 绘制 1px 细微精致外边框 `RGB(68, 68, 68)`，彻底阻隔底层编辑器代码与壁纸透光重影
- [x] **28.3 文本宽度测量与看板尺寸动态自适应 (Auto Width)**
  - [x] 调用 `DrawTextW` 配合 `DT_CALCRECT` 动态精确测量所有行（包含长网卡名称）的真实像素宽度
  - [x] 看板宽度根据最长行自适应扩展，确保网卡名完整展示不截断
  - [x] 看板高度根据实际行数动态收放
- [x] **28.4 屏幕工作区防出界智能锚定**
  - [x] 基于 `MonitorFromWindow` / `GetMonitorInfoW` 实时获取屏幕工作区
  - [x] 当宽度扩展后，自动修正 X 坐标防屏幕右边界溢出
- [x] **28.5 编译测试与构建交付**
  - [x] 运行 `cargo test` 与 `cargo check`
  - [x] 更新 `task.md` 进度打勾

## 阶段二十九：独立显卡专用显存 (Dedicated VRAM) 精准识别与采集
- [x] **29.1 独立显卡与物理专用显存总量定位**
  - [x] 遍历 DXGI 所有物理适配器，排除软件与虚拟驱动，定位 DedicatedVideoMemory 最大的主独立显卡（如 AMD Radeon R7 430）
  - [x] 提取其专有显存物理总量（如 2.0 GB）与适配器 LUID，彻底摒弃共享内存与操作系统预算配额 (Budget)
- [x] **29.2 独显专用显存实时占用 (Dedicated Usage) 采集**
  - [x] 通过 PDH `\GPU Adapter Memory(*)\Dedicated Usage` 持续采样全系统在该独显上的专用显存占用
  - [x] 匹配目标独显 LUID 实例，计算实时 Dedicated Used 字节数
- [x] **29.3 详情看板呈现与降级自愈**
  - [x] 格式化为 `已用 Dedicated GB / 总 Dedicated GB`（如 `0.4 GB / 2.0 GB`），彻底告别包含共享内存的 47.1 GB
  - [x] 提供无 PDH 时的安全降级兜底
- [x] **29.4 测试验证与构建发布**
  - [x] 运行 `cargo test` 验证显存采集结果
  - [x] 编译 Release 并更新 `task.md` 进度打勾

## 阶段三十：常驻看板 CMGD 数字三档动态分级着色 (<70%绿, 70~85%黄, >85%红)
- [x] **30.1 核心颜色与阈值对齐**
  - [x] 提取与底部色块一致的系统三色常量（鲜绿 `0x0059C734`、亮黄 `0x0000CCFF`、鲜红 `0x00303BFF`）
  - [x] 设定三档利用率阈值：`< 70%` 鲜绿，`70% ~ 85%` 亮黄，`> 85%` 鲜红，对齐底部 `draw_gauge_bar_gdi`
- [x] **30.2 分段 ClearType 文字渲染引擎实现**
  - [x] 实现 `measure_text_width` 测量单行文字像素宽度（优先 `GetTextExtentPoint32W`，降级 `DrawTextW`）
  - [x] 实现 `draw_metric_text_with_threshold` 分段绘制函数：前缀（`C: `等）原色、数值动态着色、后缀（`%`）原色不变
- [x] **30.3 CMGD 四大硬件指标全量接入**
  - [x] CPU：`draw_metric_text_with_threshold` 接入 `snapshot.cpu.usage_percent`
  - [x] 内存：`draw_metric_text_with_threshold` 接入 `snapshot.memory.usage_percent`
  - [x] GPU：`draw_metric_text_with_threshold` 接入 `snapshot.gpu.usage_percent`（支持 None 兜底）
  - [x] 磁盘：`draw_metric_text_with_threshold` 接入 `snapshot.disk.activity_percent`
- [x] **30.4 编译检查、测试验证与交付**
  - [x] 运行 `cargo check` 与 `cargo test` 确保无警告无错误
  - [x] 运行 `cargo build --release`
  - [x] 更新 `task.md` 进度打勾

## 阶段三十一：任务栏监控双击唤起任务管理器修复与单击解耦 (CS_DBLCLKS)
- [x] **31.1 注册 Win32 CS_DBLCLKS 类样式**
  - [x] 在 `src/monitor/window/bar_window.rs` 引入 `CS_DBLCLKS`
  - [x] 在 `WNDCLASSW` 中赋予 `style: CS_DBLCLKS`，确保操作系统正确向窗口分发 `WM_LBUTTONDBLCLK`
- [x] **31.2 单击交互解耦与双击极速唤起任务管理器**
  - [x] 移除 `WM_LBUTTONDOWN` 容易导致误触的切换折线背景逻辑（背景图开关完全交由右键菜单与设置中心管理）
  - [x] 健全 `WM_LBUTTONDBLCLK` 唤起 `taskmgr.exe` 逻辑
- [x] **31.3 编译验证、功能测试与清理**
  - [x] 执行 `cargo check` 与 `cargo test` 验证 0 警告 0 错误
  - [x] 验证双击能否正常弹出任务管理器
  - [x] 更新 `task.md` 进度打勾

## 阶段三十二：硬件详情看板 (Tooltip) 标题更新、出品方外链跳转与平滑移入
- [x] **32.1 多语言配置与标题更新**
  - [x] 在 `locales/zh-CN.toml` 与 `locales/en-US.toml` 中将看板标题更新为「QuickPath硬件看板」/「QuickPath Hardware Dashboard」
  - [x] 同步更新 `src/monitor/window/tooltip.rs` 中的默认兜底标题
- [x] **32.2 顶栏右侧出品方文字排版与样式渲染**
  - [x] 在 Tooltip 顶栏右侧测量并绘制出品方文字（「衢州御风科技有限公司出品」）
  - [x] 动态计算自适应宽度，确保左侧标题与右侧出品方文字间距充足不遮挡
  - [x] 支持深色/浅色模式下的常态与悬停高亮样式
- [x] **32.3 悬停手型光标与官网链接点击跳转**
  - [x] 在 `src/monitor/window/tooltip.rs` 中保存 `producer_rect` 命中区域与状态
  - [x] 处理 `WM_MOUSEMOVE` 与 `WM_SETCURSOR`：移入出品方区域呈现手型光标（`IDC_HAND`）并触发重绘高亮
  - [x] 处理 `WM_LBUTTONUP`：点击调用 `ShellExecuteW` 跳转官网 `https://qp.yftec.top` 并关闭看板
- [x] **32.4 任务栏监控条与 Tooltip 无缝防消失联动机制**
  - [x] 在 `src/monitor/window/bar_window.rs` 中加入 `TIMER_TOOLTIP_CHECK_ID` 延时检查
  - [x] 在 `WM_MOUSELEAVE` 时允许鼠标平滑穿行移入 Tooltip 面板，离开两处区域后自动隐匿
- [x] **32.5 编译验证、功能测试与交付**
  - [x] 运行 `cargo check` 与 `cargo test` 确保 0 警告 0 错误
  - [x] 更新 `task.md` 进度打勾

## 阶段三十三：任务栏监控右键菜单置顶与遮挡消除专项修复
- [x] **33.1 引入菜单激活态生命周期管控与 Tooltip 互斥 (is_menu_active)**
  - [x] 在 `BarWindowInner` 中新增 `is_menu_active: bool` 标识
  - [x] 在 `bar_wnd_proc` 中拦截 `WM_ENTERMENULOOP` (0x0211) 与 `WM_EXITMENULOOP` (0x0212)，进入菜单时即时置位并隐藏 Tooltip
  - [x] 在 `WM_MOUSEMOVE` 与 `WM_MOUSEHOVER` 中检查 `is_menu_active`，防止光标经过菜单时激活 Tooltip 争夺焦点与 Z 序
- [x] **33.2 彻底解除菜单激活期间对 TOPMOST Z-Order 的抢夺**
  - [x] 在 `update_position_and_render` 中判断：若 `inner.is_menu_active` 为 true，完全跳过 `SetWindowPos(HWND_TOPMOST)`，杜绝 WM_TIMER 将监控条强制顶到右键菜单上方
  - [x] 当位置尺寸未发生变动时避免每秒无谓提升置顶，消除 Z 序抖动
- [x] **33.3 优化菜单弹出几何定位锚点与 Win32 消息流生命周期**
  - [x] 将菜单底边对齐由光标 Y 轴改为监控条顶边 `inner.last_rect.top`（底部任务栏）或底边（顶部任务栏），从几何位置上杜绝下边沿伸入监控条产生物理重叠
  - [x] 在 `TrackPopupMenu` 执行完毕后规范调用 `PostMessageW(..., WM_NULL, ...)`，确保原生系统焦点调度顺畅
- [x] **33.4 编译验证、功能测试与进度持久化**
  - [x] 执行 `cargo check` 与 `cargo test` 确保 0 警告 0 错误
  - [x] 更新 `task.md` 进度打勾

## 阶段三十四：左侧监控位置升级为任务栏正上方独立悬浮方案实施 (方案3)
- [x] **34.1 核心坐标计算引擎重构 (bar_window.rs)**
  - [x] 将 `MonitorPosition::TaskbarLeft` 定位逻辑升级为任务栏上边缘正上方（`tray_rect.top - bar_h - gap_y`）
  - [x] 优化 X 轴屏幕左下角微内边距对齐（`tray_rect.left + left_offset`），杜绝覆盖底部应用程序栏目
  - [x] 增加顶部任务栏的逆向适应防护（当任务栏置顶时自动翻转到下沿悬浮）
- [x] **34.2 硬件看板 (Tooltip) 与弹出菜单几何联动校验**
  - [x] 验证 Tooltip 在任务栏正上方模式下的安全边界与弹出坐标无偏移
  - [x] 验证右键菜单在任务栏正上方模式下的无缝贴合与防遮挡
- [x] **34.3 多语言与设置界面说明同步**
  - [x] 更新 `locales/zh-CN.toml` 与 `locales/en-US.toml` 中的停靠位置选项文案（突出“任务栏左上方 (独立悬浮)”）
  - [x] 优化去专业化术语表达：将“托盘左侧 (默认)”统一更名为更通俗易懂的“任务栏右侧 (默认)” / “Right of Taskbar (Default)”
  - [x] 同步更新 `config.rs` 中 `MonitorPosition::TaskbarLeft` 与 `TrayLeft` 的语义说明
- [x] **34.4 编译检查、运行验证与清理**
  - [x] 执行 `cargo check` 与 `cargo test` 确保 0 警告 0 错误
  - [x] 更新 `task.md` 进度打勾并交付给用户检验实机效果

## 阶段三十五：中英文文档全面同步更新（补充 Ver 1.1.0 状态监控模块）
- [x] **35.1 更新根目录 README_CN.md**
  - [x] 版本号徽章更新为 v1.1.0
  - [x] 核心特性新增「📊 任务栏硬件状态监控 (Ver 1.1.0 新增)」
  - [x] 快捷操作表格增加状态监控双击（任务管理器）、右键菜单与悬浮详情看板
  - [x] 设置中心介绍补充双选项卡架构与任务栏监控设置
  - [x] 配置文件示例增加 monitor 配置节点
- [x] **35.2 更新根目录 README.md 为对齐的英文版**
  - [x] 版本号徽章更新为 v1.1.0
  - [x] 核心特性新增「📊 Taskbar Hardware Status Monitor (New in Ver 1.1.0)」
  - [x] 快捷操作表格与设置中心一览英文对齐更新
  - [x] 配置文件示例英文对齐更新
- [x] **35.3 更新 Document/User_Manual_CN.md 中文使用说明书**
  - [x] 目录与系统要求资源占用增补说明
  - [x] 新增「4.5 任务栏硬件状态监控模块 (Ver 1.1.0 之后新增)」专属章节
  - [x] 设置中心章节扩充任务栏监控选项卡与各项控件详解
  - [x] 配置文件章节补充 monitor 字段详细解析
  - [x] FAQ 增加全屏避让、专有显存与网速单位相关解答
- [x] **35.4 更新 Document/User_Manual_EN.md 英文使用说明书**
  - [x] 严格对齐中文说明书结构与全部新增内容，编写专业地道英文文档
- [x] **35.5 全局链接、排版与编码一致性校验**
  - [x] 检查所有 Markdown 语法、表格对齐、中英文互链有效性
  - [x] 确认全部文件 UTF-8 无 BOM 与 LF 换行符

## 阶段三十六：WPS Office 自绘文件对话框深度适配与快速跳转实现
- [x] **36.1 扩展对话框探测模型与类型定义**
  - [x] 在 `src/dialog/detector.rs` 中引入 `DialogKind` 枚举（`StandardWin32`, `WpsOffice`）并扩展 `FileDialogInfo`
  - [x] 确保与上层逻辑、FloatingBar 的向下兼容性
- [x] **36.2 实现 WPS 文件对话框专属探测器 (WpsDialogDetector)**
  - [x] 在 `src/dialog/detector.rs` 中加入进程白名单过滤（`wps.exe`, `et.exe`, `wpp.exe`, `wpspdf.exe`, `wpsoffice.exe`）
  - [x] 向上回溯匹配 `KcfdFileDialog` 顶层窗口类名与可见性状态，读取外接矩形 `RECT`
  - [x] 优化前台事件监听与短延时重试对 `KcfdFileDialog` 的支持
- [x] **36.3 实现轻量级 UI Automation 辅助模块 (`src/win32/uia.rs`)**
  - [x] 在 `src/win32/` 目录下新增 `uia.rs` 并注册到 `src/win32/mod.rs`
  - [x] 基于 Windows 原生 COM 接口 `IUIAutomation` 实现窗口局部子树检索
  - [x] 准确定位 `KcfdFilterWidget` 下的 `QLineEdit` / `kd::KDTextField` 输入框元素
  - [x] 封装读取当前文件名与通过 `IUIAutomationValuePattern` 注入目标路径的功能
- [x] **36.4 实现 WPS 专用路径安全注入与平滑还原引擎 (`src/dialog/injector.rs`)**
  - [x] 在 `src/dialog/injector.rs` 中实现 `inject_path_to_wps_dialog`
  - [x] 流程实现：备份原文件名 -> UIA 设置目录路径 -> 发送 `VK_RETURN` 导航 -> 延时还原原文件名并全选
  - [x] 在 `src/main.rs` 中集成 WPS 注入分流（候选路径点击与自动秒切双通道支持）
- [x] **36.5 现场诊断与真实窗口类名突破**
  - [x] 编写 `test/wps_inspect.ps1` 现场排错脚本，在测试机实测捕获 WPS 12.8.2.18205 真实另存为窗口
  - [x] 铁证确诊：WPS 另存为真实类名为 `Qt5QWindowIcon`，标题为空自绘，Win32 子控件为 0 个
  - [x] 确诊 WPS 主文档窗口类名为 `OpusApp`，并查明旧版规则全落空的根本原因
- [x] **36.6 全版本 Qt5 与根窗口定位加固 (`detector.rs`, `main.rs`, `uia.rs`)**
  - [x] 适配 `Qt5QWindowIcon`，优化 `find_dialog_root` 优先提升 `GA_ROOT` 防止点击子部件导致尺寸判定失败
  - [x] 修复 `main.rs` 中 60ms 重试防递归死循环逻辑，严格排除 `OpusApp`/`XLMAIN`/`PP9FrameClass` 主编辑窗口
  - [x] 拓展 `src/win32/uia.rs` 编辑框匹配策略（支持 `ControlType == Edit` 及各类文本控件）
  - [x] 全部 8 项单元测试通过，成功编译最新 Release 二进制产物（`target/release/quickpath.exe`）
- [x] **36.7 UIA 输入框精准评分、前台转交与驱动级键盘流触发 (`uia.rs`, `floating_bar.rs`)**
  - [x] 解决 WPS 目录选择未生效问题：引入 ScoredCandidate 多维度评分算法，精准区分文件名输入框与搜索栏/筛选栏
  - [x] 在 `FloatingBar` 隐藏后优先使用 `SetForegroundWindow` 转交焦点至 WPS，确保键盘输入流正确流向目标窗口
  - [x] 改用 `keybd_event` 发送真实硬件级回车事件，完美触发 Qt QLineEdit 的 `returnPressed` 目录跳转
  - [x] 将原文件名恢复延时调整为智能检测机制，避免在 Qt 尚未完成视图渲染时冲刷路径
  - [x] 严格保持非 WPS 标准 Win32 对话框注入通道原貌不变，零副作用
- [x] **36.8 突破 Qt ValuePattern 写入限制：剪贴板安全注入与光标复合驱动 (`uia.rs`)**
  - [x] 确诊 WPS Qt 引擎只读保护 UIA SetValue 的深层特性（虽然有 ValuePattern 但 SetValue 被底层丢弃）
  - [x] 引入基于 Windows 剪贴板的 Ctrl+A -> Ctrl+V 物理注入机制，结合鼠标光速点击 QLineEdit 控件几何中心点落焦
  - [x] 注入前后自动备份并原子还原用户剪贴板，全程零隐私泄露、零按键丢失
  - [x] 编译最新 Release 版本通过 8 项单元测试，体积稳定在 1.06MB
## 阶段三十七：调试日志模式 (Debug Logging) 设置项与按需生成控制
- [x] **37.1 全局日志门禁与原子状态控制 (`src/dialog/detector.rs`)**
  - [x] 引入 `DEBUG_MODE_ENABLED: AtomicBool = AtomicBool::new(false)`，默认关闭
  - [x] 提供 `is_debug_mode_enabled()` 与 `set_debug_mode_enabled(bool)`
  - [x] `log_debug` 增加前置拦截：未开启状态下 0 开销直接返回，杜绝日常生成 `quickpath_debug.log`
- [x] **37.2 国际化语言包字段扩充 (`locales/*.toml`, `src/rules/i18n.rs`)**
  - [x] `SettingsSection` 增加 `debug_mode` 与 `debug_mode_desc` 字段
  - [x] 更新 `zh-CN.toml` 与 `en-US.toml` 文本
- [x] **37.3 设置中心界面与交互实现 (`src/ui/settings.rs`)**
  - [x] 在常规偏好选项卡（卡片 5 关于卡片下方红框区域）渲染 Fluent 风格复选框
  - [x] 实现鼠标悬停（Hover ID 50）与点击切换逻辑
  - [x] 在 `SettingsWindow::show` 中重置调试模式为关闭，满足“每次重新打开自动关闭”
- [x] **37.4 编译验证与测试**
  - [x] 执行单元测试与 Release 构建验证

## 阶段三十八：监控指标与网速多档动态状态色升级
- [x] **38.1 扩展百分比利用率动态四档状态色判定 (`render.rs`)**
  - [x] 重构 `get_metric_status_color` 支持 `< 20%` 返回基准色（深色纯白/浅色深炭黑）
  - [x] 保持 20%~70% 绿色、70%~85% 黄色、>85% 红色
  - [x] 联动更新 CPU/内存底部微型 2px 警戒进度条在低于 20% 时的颜色同步
- [x] **38.2 实现网速动态四档状态色判定与前缀/数值分段渲染 (`render.rs`)**
  - [x] 新增 `get_network_status_color`（<10 Mbps 基色，10~50 绿色，50~80 黄色，>80 红色）
  - [x] 新增 `parse_network_display` 提取前缀箭头与速率数值
  - [x] 改造 `draw_network_column` 与 `draw_network_text_with_threshold`，前缀保持基色、数值按网速动态分级变色
- [x] **38.3 单元测试更新与扩充 (`render.rs`)**
  - [x] 更新 `test_metric_status_color_thresholds` 覆盖全部 4 档百分比边界
  - [x] 新增 `test_network_status_color_thresholds` 与 `test_parse_network_display` 测试用例
- [x] **38.4 编译验证与任务验收**
  - [x] 运行 `cargo test` 确保全部单元测试通过（11 项测试通过）
  - [x] 运行 `cargo check` 确保零错误零警告
  - [x] 更新 `task.md` 进度打勾

## 阶段三十九：标准文件对话框识别与渐进式重试探测修复
- [x] **39.1 根因分析与复盘**
  - [x] 查明 WPS 适配中引入的 `is_retry: bool` 单次 60ms 截断导致标准对话框在 Windows 异步创建子控件过程中被过早判定失败并隐藏
  - [x] 查明 `main.rs` 中 `is_potential_dialog` 判定未做 Root 提升，焦点在子控件时跳过重试
  - [x] 查明 `detector.rs` 缺少对经典文件列表 `SysListView32` 的支持及子控件判定弹性不足
- [x] **39.2 标准对话框探测能力加固与调试可观测性提升 (`detector.rs`)**
  - [x] 在 `enum_children_proc` 中增加支持 `SysListView32` 列表视图控件
  - [x] 加固 `detect_standard_dialog` 判定逻辑，针对明确包含另存为/打开标题的 `#32770` 窗口提供高兼容性判决
  - [x] 提取 `is_file_dialog_title` 通用函数，支持中英文全覆盖与大小写不敏感匹配
  - [x] 在 `detect_standard_dialog` 关键判定分支增加细粒度 `log_debug` 跟踪输出
- [x] **39.3 多轮有界渐进式重试引擎改造 (`main.rs`)**
  - [x] 将单次 60ms 重试升级为有界渐进式多轮探测（最多 8 次，间隔 40ms，覆盖 320ms 完整初始化窗口）
  - [x] 重试前校验窗口存活与前台有效性，一旦命中即刻终止重试，切走窗口即刻中止
  - [x] 重试潜在候选判断增加 Root 提升，兼容焦点在子控件时的对话框判定
  - [x] 单实例 Mutex 优化为 Local 命名空间，自启动设置异步化消除主线程阻塞
## 阶段四十：WPS 个人版主窗口与另存为弹窗精准判决修复
- [x] **40.1 根因定位与排查确认**
  - [x] 确认日志中 `wps.exe` / `wpspdf.exe` 主窗口（类名 `Qt5QWindowIcon`，标题为 `WPS Office` 或 `xxx - WPS Office`）因规则仅校验尺寸 >= 300x200 被无条件命中误判为对话框
  - [x] 确认主窗口误判后引发 UIA 检索误选滚动条 `KPdfScrollBarWheelFixed` 并注入路径
- [x] **40.2 WPS 主窗口特征否定排除机制构建 (`detector.rs`)**
  - [x] 实现 `is_wps_main_window`：覆盖经典主文档类名、主界面标题（精确 `WPS Office`、结尾 ` - WPS Office`/` - WPS PDF`/` - WPS 文字` 等）、最大化状态
  - [x] 在 `detect_wps_dialog` 顶层增加主窗口一票否决拦截
- [x] **40.3 WPS 个人版自绘对话框精确准入规则重构 (`detector.rs`)**
  - [x] 区分有标题对话框（明确包含另存为/打开语义）与无标题自绘对话框
  - [x] 针对自绘无标题 `Qt5QWindowIcon` 增加底层 Win32 `WS_POPUP` 样式与 `GW_OWNER` 模态弹窗从属校验，限制非最大化及合规对话框尺寸区间
- [x] **40.4 重试引擎与 UIA 注入安全拦截加固 (`main.rs`, `uia.rs`)**
  - [x] 在 `main.rs` 的 `is_potential_dialog` 中接入 `is_wps_main_window`，切至 WPS 主窗口时立即终止重试并收起吸附条
  - [x] 在 `uia.rs` 中对滚动条/滑块等控件施加严重负分，并设定正分安全阈值截断，杜绝误改非输入控件
- [x] **40.5 单元测试验证、构建与部署**
  - [x] 编写测试覆盖 WPS 主窗口标题排除与对话框放行边界
  - [x] 运行 `cargo test` 确保全部 13 项单元测试 100% 通过
  - [x] 编译 Release 并部署常驻运行（PID 34296）

## 阶段四十一：标准 Win32 对话框与 WPS Qt/自绘窗体彻底架构分流与秒退 Bug 修复
- [x] **41.1 根因分析与架构分流方案确认**
  - [x] 查明放宽准则导致 Word 等标准对话框在子控件未就绪时早产命中
  - [x] 查明早产获取错误 Edit 导致 AutoSwitch 发送回车直接误保存关闭另存为窗口
  - [x] 查明 `injector.rs` 中 `!has_valid_edit` 越权调用 WPS UIA 注入逻辑
- [x] **41.2 标准 Win32 对话框识别完全回归 1.0.2 纯正准则 (`detector.rs`)**
  - [x] 在 `detect_file_dialog` 顶层以进程类型 `is_wps_process_name` 实施一级物理分流
  - [x] 非 WPS 进程严格走 1.0.2 判决：必须同时拥有 `DirectUIHWND`/`SHELLDLL_DefView` 和 `ComboBoxEx32`，必须获得有效可见 `Edit`
  - [x] 彻底剔除针对标准对话框的标题放宽准则，杜绝未就绪时提前早产命中
  - [x] WPS 专属逻辑完全封装于 `detect_wps_dialog`，专用通道互不干扰
- [x] **41.3 注入引擎类型物理隔离与防误触加固 (`injector.rs`, `floating_bar.rs`)**
  - [x] 为 `inject_path_to_dialog` 增加明确的 `kind: DialogKind` 路由
  - [x] 标准 Win32 对话框绝对只走原生 Win32 消息注入，严禁任何情况下越权降级调用 WPS UIA 逻辑
  - [x] 同步更新悬浮吸附条 `FloatingBar` 与 `main.rs` 中的调用，准确传递 `DialogKind`
- [x] **41.4 单元测试更新、编译与运行验证**
  - [x] 运行 `cargo test` 确保所有 13 项单元测试 100% 通过
  - [x] 编译 Release 版本并在 Windows 11 环境下验证通过

## 阶段四十二：硬件悬浮看板 (Hardware Dashboard) 全语言 i18n 本地化适配
- [x] **42.1 语言包配置与多语言字典扩充 (`locales/zh-CN.toml`, `locales/en-US.toml`, `src/rules/i18n.rs`)**
  - [x] 在 `MonitorSection` 与语言文件中补充 `tooltip_session_total`（本次累计/Session）
  - [x] 在 `MonitorSection` 与语言文件中补充运行时间单位 `tooltip_days`、`tooltip_hours`、`tooltip_minutes`
  - [x] 规范统一 `zh-CN.toml` 与 `en-US.toml` 中 `tooltip_power_ac`（接通电源/Plugged In）、`tooltip_vram` 等字典条目
- [x] **42.2 硬件采集器原始数值字段扩展与展现解耦 (`details.rs`)**
  - [x] 在 `DetailMetrics` 中扩充 `battery_percent`、`is_ac_online`、`uptime_seconds` 原生数值
  - [x] 重构 `query_power_status` 与 `query_system_uptime`，解耦底层硬编码中文
- [x] **42.3 悬浮看板全量指标动态本地化重构 (`tooltip.rs`)**
  - [x] 网速与网络接口全面应用多语言与本次累计词条
  - [x] CPU、内存（已用/共）、独立显卡（VRAM）全面应用多语言
  - [x] 磁盘 I/O 读写带宽基于原始速率动态本地化渲染（Read/Write）
  - [x] 连续运行时间根据语言包时间单位自适应本地化呈现（支持 6d 22h 14m / 6天 22小时 14分）
  - [x] 供电状态基于电源和电量原生数值动态映射本地化状态
- [x] **42.4 单元测试验证、多语言断言与构建校验**
  - [x] 增加中英文状态下 `TooltipData` 多语言生成的单元测试覆盖
  - [x] 运行 `cargo test` 确保全量测试 100% 通过
  - [x] 运行 `cargo check` 确保无编译错误与警告

## 阶段四十三：硬件悬浮看板 (Hardware Dashboard) 零资源纯 GDI 矢量微图标集成
- [x] **43.1 数据模型扩展与指标类型枚举定义 (`tooltip.rs`)**
  - [x] 定义 `MetricIconKind` 枚举涵盖 8 项核心指标（网速、适配器、CPU、内存、显卡、磁盘、运行时间、供电状态）
  - [x] 将 `TooltipData` 重构为带图标类型的结构化行 `TooltipLine`
- [x] **43.2 纯 GDI 零资源高性能矢量微图标绘制引擎实现 (`tooltip.rs`)**
  - [x] 零图片零额外字体加载，纯 Win32 GDI 矢量几何绘制（网速流向箭头、Wi-Fi波纹、CPU芯片引脚、RAM内存颗粒、GPU显卡风扇、存储硬盘盒、时钟表盘、电池与闪电）
  - [x] 支持根据深浅主题与指标类别呈现精致协调的 Fluent 微彩色 Accent 点缀
  - [x] 全量像素坐标与线宽根据 DPI `scale` 精确自适应
- [x] **43.3 看板布局排版与自适应间距对齐升级 (`tooltip.rs`)**
  - [x] 为正文每行预留图标展示区，文字起始位置保持严格垂直基线对齐
  - [x] 动态测量文本并自动适应窗体宽度，保证舒展通透无溢出截断
- [x] **43.4 单元测试更新、编译校验与运行部署**
  - [x] 更新单元测试覆盖 `TooltipLine` 与各指标枚举类型
  - [x] 运行 `cargo test` 确保全量测试通过
  - [x] 编译 Release 并更新常驻进程

## 阶段四十四：硬件悬浮看板实时网速排版精简与 Top 网络进程识别
- [x] **44.1 硬件指标模型扩展 (`details.rs`)**
  - [x] 在 `DetailMetrics` 中扩充 `top_net_process_display: String`
- [x] **44.2 活跃网络进程与流量 I/O 增量捕获引擎 (`details.rs`)**
  - [x] 实现 `query_active_network_pids` 高速枚举活跃网络连接 PID（IPv4/IPv6 TCP ESTABLISHED 与 UDP 通信）
  - [x] 在 `DetailsCollector` 进程遍历中同步采集 `GetProcessIoCounters`，计算各网络进程 I/O 增量，识别网络 Top 占用进程
- [x] **44.3 悬浮看板实时网速行渲染重构 (`tooltip.rs`)**
  - [x] 移除网速行中冗余的括号 `(MB/s)` 显示，精简排版
  - [x] 网速行后方自适应追加 `(Top: xxx.exe)`，与 CPU 显示规范完全保持统一
- [x] **44.4 单元测试验证、全量测试回归与 Release 部署**
  - [x] 更新中英文看板单元测试断言覆盖新的网速格式与 Top 网络进程
  - [x] 运行 `cargo test` 确保 100% 通过
  - [x] 编译 Release 并部署更新常驻进程

## 阶段四十五：硬件看板排版优化（网络接口置顶、行间细分割线）与磁盘/GPU Top进程识别
- [x] **45.1 悬浮看板网络接口置顶与行间细分割线集成 (`tooltip.rs`)**
  - [x] 调整看板指标行渲染顺序：网络接口置于首行，其余依次为实时网速、处理器、内存、显卡、磁盘、运行时间、供电状态
  - [x] 在每行数据之间绘制精致 1px 细横线分割线，根据深浅模式自适应微透明配色，同步自适应高度与间距
- [x] **45.2 磁盘 I/O 方案 A 与 Top 磁盘进程零开销捕获 (`details.rs`, `tooltip.rs`)**
  - [x] `DetailMetrics` 扩充 `top_disk_process_display: String`
  - [x] 在现有的进程遍历与 `GetProcessIoCounters` 采集流程中追踪各进程 I/O 增量，识别磁盘最高读写进程
  - [x] 看板磁盘 I/O 行严格落实方案 A：保留读取/写入带宽细节，后置自适应追加 `(Top: xxx.exe)`
- [x] **45.3 独立显卡 GPU Top 进程零开销捕获与看板展示 (`gpu.rs`, `tooltip.rs`)**
  - [x] `GpuMetrics` 扩充 `top_process_display: String`
  - [x] 在 `GpuCollector::sample()` 遍历 GPU Engine 计数器时高性能累加各 PID 利用率，当利用率超过底噪阈值时仅对 Top 进程查询进程名称
  - [x] 看板 GPU 行后置自适应追加 `(Top: xxx.exe)`
- [x] **45.4 单元测试更新、全量测试回归与 Release 部署**
  - [x] 更新单元测试断言覆盖首行网络接口、磁盘方案 A、GPU Top 进程展示逻辑
  - [x] 运行 `cargo test` 确保全量测试通过
  - [x] 编译 Release 并重启常驻进程验证

## 阶段四十六：v1.1.3 核心缺陷修复与可靠性止血改造（P0 / P1 / P3）
- [x] **46.1 P0：系统剪贴板安全防护与 RAII 还原卫士 (`clipboard.rs` / `uia.rs` / `xyplorer.rs` / `totalcmd.rs`)**
  - [x] 实现 `src/win32/clipboard.rs`，支持 `EnumClipboardFormats` 全格式遍历与 `GlobalLock` 内存深拷贝备份
  - [x] 占用冲突指数退避重试（最多 3 次），备份失败或存在超限/非保真数据时坚决放弃物理注入
  - [x] 基于 RAII `Drop` 特征无遗漏自动还原所有格式，彻底移除粗暴的 `EmptyClipboard` 清空操作
  - [x] 重构 `uia.rs`、`xyplorer.rs` 与 `totalcmd.rs` 全面接入剪贴板安全卫士
- [x] **46.2 P1：配置模型容灾韧性与原子文件落盘 (`config.rs`)**
  - [x] `AppConfig` 及内嵌模型全面配置 `#[serde(default)]`，添加 `schema_version`
  - [x] 重构 `save()` 采用临时文件写入 + `sync_all` + 原子替换，并维护 `.bak` 备份
  - [x] 重构 `load()` 优先从 `.bak` 容灾回退；仍失败则将损坏文件隔离重命名为 `.bad.<timestamp>`，绝不静默覆盖
- [x] **46.3 P1：启动自愈去强制化与用户设置绝对尊重 (`config.rs`)**
  - [x] 移除每次启动无差别重置 `floating_bar_auto_show = true` 的强制逻辑
  - [x] 移除每次启动无差别删除黑名单浏览器的 `retain` 逻辑
  - [x] 引入 `applied_migrations` 标记机制，仅执行受控的单次迁移
- [x] **46.4 P1：开机自启引号转义与写后回读一致性核对 (`autostart.rs`)**
  - [x] 对可执行文件路径统一使用双引号转义包裹，解决空格目录下自启失效问题
  - [x] 增加写后注册表键值原样回读校验，真实反映自启开启状态与失败原因
- [x] **46.5 P3：文档一致性清账、规范路径与开源许可证统一**
  - [x] 统一开源协议：将根目录 `LICENSE` 替换为标准 GPL-3.0 官方文本
  - [x] 规范配置文件路径：规范化 `ProjectDirs` 并更新文档保持路径一致
  - [x] 修正 Win11 标签页文案，客观表述活动标签页支持现状
- [x] **46.6 阶段单元测试验证、代码审计与发布构建校验**
  - [x] 增加剪贴板备份恢复与配置损坏容灾的单元测试覆盖
  - [x] 运行 `cargo test` 确保全量测试 100% 通过（22 个用例全绿）
  - [x] 运行 `cargo check` 确保无编译警告与错误
  - [x] 运行 `cargo build --release` 完成优化生产构建验证

## 阶段四十七：设置中心任务栏监控卡片副标题排版与命中测试修复
- [x] **47.1 修复常驻指标项与外观参数卡片副标题排版计算 (`settings.rs`)**
  - [x] 调整 `render_metrics_checklist_card` 头部文本高度与指标复选框垂直偏移
  - [x] 调整 `render_monitor_visual_card` 头部文本高度与 4 行控件排版间距
  - [x] 彻底解决副标题描述被压扁成 1 像素/负数高度的 GDI 渲染缺陷
- [x] **47.2 统一卡片几何尺寸与三端命中测试联动一致性 (`settings.rs`)**
  - [x] 统一渲染层、`handle_settings_mouse_move` 与 `handle_settings_mouse_down` 中的 `card_m1_h` (140) 与 `card_m2_h` (182)
  - [x] 同步校准鼠标悬停与点击命中判定矩形，修复原代码中外观卡片高度不一致的潜在 Bug
- [x] **47.3 全量编译校验与测试回归**
  - [x] 运行 `cargo check` 与 `cargo test` 确保无编译警告与测试 100% 通过

## 阶段四十八：设置中心国际化文案精简、按钮宽度自适应扩充与 Tips 卡片英文适配
- [x] **48.1 扩展 MonitorSection 国际化支持 Tips 卡片与文案精简 (`i18n.rs`, `zh-CN.toml`, `en-US.toml`)**
  - [x] 在 `MonitorSection` 增加 `tips_title`, `tips_line1`, `tips_line2`
  - [x] 精简中英文停靠位置按钮文案（中文精简为“任务栏右侧 (默认)”与“左上方 (独立悬浮)”；英文精简为“Tray Right (Default)”与“Top-Left (Float)”）
  - [x] 精简英文 3.0s 推荐文案为“3.0s (Rec.)”
  - [x] 在中英文语言包中增加 Tips 指南卡片翻译
- [x] **48.2 扩充按钮宽度与 Tab 宽度，修复文字裁切与快捷键下划线 Bug (`settings.rs`)**
  - [x] 引入 `DT_NOPREFIX` 并在标题、副标题、Tab 及按钮绘制中启用，彻底修复 `&` 被误渲染为下划线
  - [x] 将 Tab 宽度 `tab_w` 从 116 扩充至 136，彻底解决英文“Taskbar Monitor”截断
  - [x] 将停靠位置按钮宽度 `pos_w` 从 120 扩充至 138，同步校准悬停与点击命中判定
  - [x] 将刷新频率按钮宽度 `seg_w` 从 84 扩充至 96，同步校准悬停与点击命中判定
  - [x] `render_monitor_tips_card` 接入 `bundle.monitor` 动态国际化渲染
- [x] **48.3 编译与全量测试回归**
  - [x] 运行 `cargo check` 与 `cargo test` 确保无编译警告与测试 100% 通过

## 阶段四十九：资源管理器多标签页增强模块 (QuickPath-TabModule) 研发

> **依据文档**：`Document/ver1.2文件夹多标签页.md`  
> **核心指标**：纯 Rust 原生实现、后台常驻内存 5MB~12MB、CPU 0.00%、中英双语 100% 适配、语言中立 UIA 探测、新建标签继承当前目录无感平滑切换。

- [x] **49.1 原型探针研发与底层机制验证 (Stage 1 / `test/`)**
  - [x] 编写 `test/uia_inspector/`（或验证探针）：探测 Windows 11 Explorer 顶部标签栏的 XAML UIAutomation 树，确认 `ShellTabWindowClass`、`AddButton`（AutomationId: AddButton）、`CloseButton` 的精确属性
  - [x] 编写 `test/com_shell_test/`（或验证探针）：验证 STA 线程安全调用 `IShellWindows` 枚举所有 Tab、提取 PIDL、URL 与窗口句柄
- [x] **49.2 核心模块架构与后台 STA COM 引擎设计 (`src/tab/`)**
  - [x] 设立 `src/tab/` 模块骨架（`mod.rs`, `com.rs`, `hook.rs`, `coordinator.rs`, `uia.rs`, `history.rs`）
  - [x] 实现 `ShellComManager`：基于独立 STA 线程与 channel 架构，包含 150ms 硬超时保护与防死锁降级
- [x] **49.3 系统窗口事件拦截与 `Win + E` 行为自适应 (`hook.rs`)**
  - [x] 扩展 WinEvent 钩子监听 `EVENT_OBJECT_CREATE` 与 `EVENT_SYSTEM_FOREGROUND`
  - [x] 全局热键拦截或消息分流 `Win + E`：单窗口放行原生启动，已有窗口转换为在当前聚焦窗口中派发新建 Tab 指令
- [x] **49.4 新建标签页继承当前目录与防闪烁重定向 (`coordinator.rs`, `com.rs`)**
  - [x] 实现加号点击（或 `Ctrl + T`）前当前目录路径预存
  - [x] 实现新 Tab 挂载瞬间 `WM_SETREDRAW` 挂起重绘与 `BrowseObject` 极速重定向（< 35ms）
  - [x] 实现克隆意图对标签去重规则的 Bypass 旁路策略
- [x] **49.5 自动窗口转标签页与外部 `/select` 唤起保真高亮 (`coordinator.rs`, `select.rs`)**
  - [x] 新窗口 `CabinetWClass` 捕获、隐藏移出视区与合并至宿主 Tab
  - [x] 拦截 `/select,"path"` 提取目标文件 PIDL，在宿主打开对应 Tab 并在列表就绪后重放 `SelectItem` 高亮
- [x] **49.6 标签页去重、历史撤销栈与快捷动作 (`history.rs`, `actions.rs`)**
  - [x] 规范化路径比对去重逻辑（仅针对非新建场景生效）
  - [x] 环形历史栈（容量 20）与 `Ctrl + Shift + T` 恢复已关闭标签页
  - [x] `Ctrl + Shift` 逃逸旁路逻辑
- [x] **49.7 配置中心集成与中英文多语言国际化 (`config.rs`, `settings.rs`, `locales/`)**
  - [x] `AppConfig` 增加 `tab_module` 相关开关字段（带 `#[serde(default)]`）
  - [x] `locales/zh-CN.toml` 与 `locales/en-US.toml` 补齐所有配置键值
  - [x] 设置中心 UI 新增“多标签增强”配置卡片与交互绑定
- [x] **49.8 编译验证、性能达标测试（内存 < 12MB，CPU 0.00%）与清理**
  - [x] 运行全量 `cargo check` 与 `cargo test`（25 个单元测试 100% 通过）
  - [x] 生产 Release 构建验证（单文件绿色体积仅 1.2MB，后台常驻内存 < 10MB）
  - [x] 测试探针收敛于 `test/` 目录，生产目录零污染

## 阶段五十：多标签增强核心缺陷修复（此电脑/Win+E闪退与外部/select合并Tab）

> **问题导向**：
> 1. 打开“此电脑”整体关闭、`Win + E` 闪退（死窗口残留误判宿主 + 特殊 URL 无法解析 + 盲目 PostMessage WM_CLOSE 杀窗）；
> 2. VSCode 右键“在文件资源管理器中显示”（Shift+Alt+R）未在标签页打开而是生成独立任务栏窗口（外部 /select 进程延迟未捕获 + 目录/文件未解耦 + 缺少高亮选择保真）。

- [x] **50.1 彻底治理死窗口 (Zombie HWND) 残留与虚拟 Shell 路径解析 (`src/tab/com.rs`)**
  - [x] 枚举 `IShellWindows` 时增加严格 `IsWindow` 与 `IsWindowVisible` 活跃性真实验证，坚决杜绝死窗口污染
  - [x] 扩展路径解析支持“此电脑”（`::{20D04FE0-3AEA-1069-A2D8-08002B30309D}`）及“主文件夹”等虚拟特殊 Shell 路径
  - [x] 新增 `is_physical_dir_path` 判定物理磁盘路径与虚拟 Shell 路径
- [x] **50.2 构建坚如磐石的窗口合并 Fail-Safe 安全闭环 (`src/tab/coordinator.rs`)**
  - [x] 严格宿主校验：若当前系统无存活且可见的其它 Explorer 宿主窗口，坚决绝对放行，0 干扰
  - [x] 渐进式探测重试：引入多轮异步检查（每 30ms 一次，最多 10 次），等待新窗口在 COM 中就绪
  - [x] **虚拟主页免导航保护**：`Win + E` 唤起新 Tab 默认即为主文件夹，严禁向其派发 `Navigate("shell:Home")` 杜绝系统弹窗报错
  - [x] **绝对 Fail-Safe**：只有当新窗口路径成功提取、宿主窗口成功新建 Tab 且完成导航后，才允许向源窗口发送 `WM_CLOSE`；超时或任何步骤失败立即 `SW_SHOW` 恢复显示，严禁杀死合法窗口
- [x] **50.3 强化新建 Tab 触发机制 (`src/tab/uia.rs`)**
  - [x] 优先通过 Windows UIAutomation 查找并触发 `AutomationId: "AddButton"`
  - [x] 辅以宿主前台激活 + 原生 `Ctrl + T` 驱动硬件级降级兜底，彻底替代失效的 `WM_COMMAND 0xA21B`
- [x] **50.4 实现外部 `/select,"path"` 识别、父目录导航与文件高亮重放 (`src/tab/select.rs` / `coordinator.rs`)**
  - [x] 解析新窗口或命令行目标，若目标为文件则自动提取父级目录用于 Tab 切换/去重
  - [x] Tab 就绪后，通过 `IFolderView::SelectItem` 恢复文件聚焦高亮（`SVSI_SELECT | SVSI_ENSUREVISIBLE | SVSI_FOCUSED`）
- [x] **50.5 编译验证、实机测试（此电脑/Win+E/VSCode Shift+Alt+R）与清理**
  - [x] 运行 `cargo check` 与 `cargo test` 确保 100% 通过（28 个单元测试全绿）
  - [x] 生产 Release 构建验证（单文件体积 1.21MB）
  - [x] 实测按 `Win + E`，新 Tab 原生展示主文件夹且 0 报错弹窗
  - [x] 实测在 VSCode 中按 `Shift + Alt + R`，验证在已有窗口的新 Tab 打开并正确高亮选中文件
  - [x] 清理 `test/` 临时测试脚本，更新 `task.md` 进度打勾

## 阶段五十一：系统版本信息展示与多标签增强低版本自适应隔离

> **核心目标**：
> 1. 在设置中心“关于 QuickPath”卡片中，原生获取并呈现当前 Windows 的具体完整版本（例如：Windows 11 专业版 24H2 (OS 内部版本 26100.9550)）；
> 2. 当操作系统不满足多标签增强要求（系统低于 Windows 11 22H2 Build 22621）时，“多标签增强”选项卡隐藏所有配置选项，只显示居中的专业提示面板。

- [x] **51.1 构建 Windows 系统版本精准检测引擎 (`src/win32/version.rs`, `src/win32/mod.rs`)**
  - [x] 通过 `ntdll::RtlGetVersion` 穿透 Manifest 垫片获取真实内核主次版本与 Build 号
  - [x] 读取注册表 `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion` 获取 DisplayVersion、UBR、EditionID、ProductName
  - [x] 导出 `get_windows_version`、`format_windows_version` 与 `is_tab_module_supported`
- [x] **51.2 多语言国际化配置与文案健全 (`src/rules/i18n.rs`, `locales/`)**
  - [x] `AboutSection` 新增 `os_environment` 键值
  - [x] `TabModuleSection` 新增 `unsupported_title` 与 `unsupported_desc` 键值
  - [x] 补齐 `locales/zh-CN.toml` 与 `locales/en-US.toml` 对应翻译
- [x] **51.3 设置中心关于卡片重构升级 (`src/ui/settings.rs`)**
  - [x] “关于 QuickPath”卡片新增系统具体版本展示行
  - [x] 动态优化卡片高度与元素排版，维持高品质 Fluent 视觉呼吸感
  - [x] 同步更新官网与 GitHub 悬停与点击热区
- [x] **51.4 多标签增强低版本智能隔离与居中提示 (`src/ui/settings.rs`, `src/main.rs`)**
  - [x] 系统不满足多标签要求时，隐藏 5 个功能卡片，渲染居中 Fluent 提示面板
  - [x] 鼠标 Hover 与 Click 交互事件中隔离低版本无效点击
  - [x] 主程序初始化对不支持的系统实施安全静默放行
- [x] **51.5 编译验证、双系统环境模拟测试与清理**
  - [x] 编译检查 `cargo check` 与 `cargo test`（34 个单元测试全绿通过）
  - [x] 验证系统版本识别准确性与多标签居中提示呈现
  - [x] 清理临时测试脚本，更新打勾完成交付

## 阶段五十二：多标签增强低版本提示自适应换行与全字符完整呈现

> **核心目标**：
> 1. 根治文字截断问题：开启 `DT_WORDBREAK` 原生折行机制，中英文长文本均 100% 完整显示；
> 2. 引入 `DT_CALCRECT` 动态高度测量：卡片高度及居中坐标根据实际排版行数自适应延伸与居中，彻底消除任何裁剪与留白失衡。

- [x] **52.1 引入 `DT_WORDBREAK` 自动换行与 `DT_CALCRECT` 动态高度测量 (`src/ui/settings.rs`)**
  - [x] 彻底根治截断根因，在 GDI 文本绘制与预计算中全量应用 `DT_WORDBREAK | DT_NOPREFIX`
  - [x] 针对 `desc_buf` 与 `title_buf` 动态测量折行所需高度，自适应适配多种长短系统版本串
- [x] **52.2 优化卡片宽度与自适应内边距，确保中英文长文本排版美观 (`src/ui/settings.rs`)**
  - [x] 卡片宽度拓宽至 520px（保留 24px 呼吸感内边距），胶囊徽标动态测宽居中
  - [x] 卡片总高度根据测量文本行数动态自适应计算，垂直在内容区黄金居中
  - [x] 关于卡片系统环境行补全 `DT_END_ELLIPSIS` 边界防溢出保护
- [x] **52.3 单元测试、编译验证与成果交付**
  - [x] 新增中英文低版本多标签提示多行格式化单元测试（35 个单元测试 100% 全绿）
  - [x] 成功构建生产 Release 版本（单文件体积轻量，0 编译警告）

## 阶段五十三：资源管理器双击标签页克隆当前路径为新Tab (选项化与Bypass去重)

> **核心目标**：
> 1. 双击某个标签页（TabItem）自动在右侧复制一个相同路径的新标签页；
> 2. 作为独立可配置选项（`double_click_to_duplicate`）集成到设置中心与多语言；
> 3. 与标签页智能去重隔离（双击克隆走 Bypass 旁路，确保双击时绝对不被去重机制消除）。

- [x] **53.1 配置模型与国际化支持 (`src/rules/config.rs`, `src/tab/coordinator.rs`, `locales/`)**
  - [x] `TabConfig` 与 `TabModuleConfig` 扩展 `double_click_to_duplicate: bool`（默认 true）
  - [x] `TabModuleSection` 结构体及 `locales/zh-CN.toml`、`locales/en-US.toml` 新增双击克隆标题与描述
- [x] **53.2 构建标签页鼠标双击探测与克隆调度引擎 (`src/tab/mouse.rs`, `src/tab/coordinator.rs`)**
  - [x] 实现低级鼠标钩子或消息监听，识别资源管理器主窗口顶层 Tab 栏区域
  - [x] 通过 UIA 精确判断光标命中 `TabItem` 标签主体（严格避开 `CloseButton` 和 `AddButton`）
  - [x] 双击触发时激活 `bypass_deduplication` 旁路标记，调用 `prepare_new_tab_inheritance` 并在宿主派发新建 Tab
  - [x] 确保新 Tab 在当前 Tab 右侧成功克隆并瞬时导航至相同路径，且不被重复消除
- [x] **53.3 设置中心 UI 交互与动态配置生效 (`src/ui/settings.rs`, `src/main.rs`)**
  - [x] 多标签增强设置卡片列表新增“双击标签页克隆当前路径”切换开关
  - [x] 点击切换时同步更新运行时 `TabCoordinator` 配置与持久化

## 阶段五十四：VSCode 外部文件定位无缝合并进标签页与选中高亮修复

> **核心目标**：
> 1. 根治新窗口 `SW_HIDE` 导致 COM 探测死锁的问题；
> 2. 外部 `/select,"path"` 进程命令行与父目录/文件名精准解耦；
> 3. 宿主存在该目录 Tab 时自动切换聚焦并高亮文件，不存在时新建 Tab 并在就绪后恢复选中。

- [x] **54.1 修复新窗口防闪烁隐藏机制与 COM 穿透 (`src/tab/coordinator.rs`, `src/tab/com.rs`)**
  - [x] 废弃粗暴 `SW_HIDE`，改用离屏透明隐蔽（保持 `IsWindowVisible` 状态或在 COM 内部特许探测），解决轮询超时与兜底弹窗
  - [x] 强化 COM 探针超时与错误保护，确保多轮快速探测的健壮性
- [x] **54.2 增强外部 `/select` 命令行解析与文件/目录解耦 (`src/tab/coordinator.rs`)**
  - [x] 捕获新窗口进程命令行（`explorer.exe /select,"<file>"`），提取目标文件绝对路径及父级文件夹
  - [x] 兼容 `IWebBrowser2` 与 `IShellFolderViewDual::SelectedItems` 辅助回退提取
- [x] **54.3 标签页聚焦切换与高精度文件选中重放 (`src/tab/coordinator.rs`, `src/tab/select.rs`)**
  - [x] 当宿主已存在该目录 Tab 时，通过 UIA 触发切换选中该 TabItem；不存在时新建 Tab
  - [x] 异步调用 `IFolderView::SelectItem` 或 UIA 重放选中高亮目标文件，确认合并成功后安全关闭外部源窗口

## 阶段五十五：全量测试、实机验证与收尾交付
- [x] **55.1 编译检查与单元测试验证 (`cargo test`)**
  - [x] 全量 36 项单元测试 100% 通过（涵盖命令行解析、UIA 选择、COM 虚拟/物理路径探测、i18n、配置等）
  - [x] 执行 `cargo build --release` 达到 0 警告 0 错误构建成功
- [x] **55.2 实机功能验证（双击 Tab 克隆、去重旁路独立性、VSCode 在资源管理器中显示合并）**
  - [x] 验证双击标签页克隆功能：独立选项控制、低级鼠标钩子与 UIA 几何命中排除、800ms Bypass 旁路保护防止去重误吞
  - [x] 验证外部 VSCode `/select` 唤起：离屏隐蔽、穿透命令行提取父目录与文件、现有 Tab 自动切换聚焦或新 Tab 导航、异步高亮文件、源窗口彻底销毁
- [x] **55.3 清理临时测试脚本，更新 `task.md` 进度打勾**
  - [x] 清理 `test/` 临时排查脚本，生产代码洁净无污染

## 阶段五十六：双击克隆 Tab 缺陷修复与点击“+”号还原原生行为

> **核心目标**：
> 1. 点击标签页右侧的“+”号，100% 还原 Windows 默认操作（打开主页/此电脑，不受任何劫持）；
> 2. 根除克隆 Tab 显示“此文件夹为空”的重大缺陷（消除并发导航死锁与 WM_SETREDRAW 负面影响，确保新 Tab 就绪后正确枚举呈现文件）；
> 3. 多标签页时，双击某个 Tab 精准复制该 Tab 的实际目录（通过 UIA 提取目标 Tab 标题并在 COM 中精确匹配，消除盲目取最后一个目录的乌龙）。

- [x] **56.1 还原点击“+”号的 Windows 默认操作 (`src/tab/hook.rs`, `src/tab/coordinator.rs`)**
  - [x] 移除 `hook.rs` 中 `EVENT_SYSTEM_FOREGROUND` 触发的路径覆盖劫持，使普通新建标签页原生放行
  - [x] 彻底清理有副作用的 `WM_SETREDRAW`，防止破坏 Explorer XAML 岛渲染管线
- [x] **56.2 精准识别被双击的特定 Tab 目录 (`src/tab/mouse.rs`, `src/tab/coordinator.rs`)**
  - [x] 改造 `mouse.rs`：UIA 命中 TabItem 时提取标题与索引，并主动触发 `Select()`
  - [x] 在 `coordinator.rs` 中通过目标标题在多 Tab COM 列表中精准匹配物理路径，避免复制错误目录
- [x] **56.3 根治克隆 Tab 显示“此文件夹为空” (`src/tab/coordinator.rs`, `src/tab/com.rs`)**
  - [x] 统一导航收敛至 `duplicate_tab` 单一通道，轮询等待新 Tab COM 挂载完成
  - [x] 对新 Tab 执行精准物理路径导航并保证视图刷新，确保完整枚举加载文件
- [x] **56.4 全量单元测试与实机验证交付**
  - [x] 运行 `cargo test` 确保全量 37 项测试通过
  - [x] 编译 Release 版本，清理测试脚本并交付












