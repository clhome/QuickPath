# WindPath 项目重命名与迁移任务清单

> 依据《QuickPath 更名为 WindPath（御风路径）标准化迁移手册》，将 QuickPath 完整迁移更名为 WindPath。

## 任务执行进度

- [x] 1. 基础配置与清单更新
  - [x] 1.1 更新 `Cargo.toml`（包名 `windpath`，主页与仓库地址更新）
  - [x] 1.2 更新 `build.rs`（文件描述、程序集名称 `WindPath.exe`，元数据及注释）
  - [x] 1.3 更新 `app.manifest`（AssemblyIdentity name 为 `WindPath`）
- [x] 2. 底层系统标识与通信管道替换
  - [x] 2.1 替换单实例互斥量 Mutex 名称为 `Local\WindPath_SingleInstance_Mutex`
  - [x] 2.2 替换主窗口、托盘、设置中心、悬浮条、任务栏监控等所有 Win32 窗口类名与标题
  - [x] 2.3 统一替换内部 Windows 自定义消息宏前缀 (`WM_WINDPATH_*`)
  - [x] 2.4 替换各后台 Worker 线程名称及调试日志文件名 (`windpath_debug.log`)
  - [x] 2.5 替换剪贴板探测类名与测试用专属格式标识
- [x] 3. 老用户数据静默迁移与开机自启动清理
  - [x] 3.1 编写配置静默迁移逻辑（从 `%APPDATA%\QuickPath` 迁移到 `%APPDATA%\WindPath`，兼容便携版 `quickpath.json` -> `windpath.json`）
  - [x] 3.2 改造 `autostart.rs`：开机自启任务与注册表项更名为 `WindPath`，启动时主动检测并清除旧版 `QuickPath` 残留自启项
- [x] 4. 业务逻辑、多语言与 UI 文案替换
  - [x] 4.1 更新 `locales/zh-CN.toml` 与 `locales/en-US.toml` 语言包
  - [x] 4.2 更新 `src/ui/settings.rs`、`src/ui/tray.rs`、`src/ui/floating_bar.rs`、`src/monitor/` 中的品牌展示文案与 GitHub 链接
  - [x] 4.3 更新 `src/rules/version.rs`、`src/rules/i18n.rs`、`src/win32/updater.rs` 中的版本检查逻辑与测试用例
  - [x] 4.4 检查更新 `src/dialog/`、`src/managers/`、`src/tab/`、`src/win32/` 中的相关引用
- [x] 5. 打包构建与 CI/CD 脚本迁移
  - [x] 5.1 重命名并更新 WiX MSI 脚本（`quickpath.wxs` -> `windpath.wxs`，更新 `build-msi.ps1`）
  - [x] 5.2 更新 MSIX 配置（`AppxManifest.xml` 及 `build-msix.ps1`）
  - [x] 5.3 更新 GitHub Actions 流水线（`.github/workflows/release.yml` 与 `ci.yml`）
- [x] 6. 门面文档与品牌过渡说明
  - [x] 6.1 更新 `README.md`、`README_CN.md`、`Document/README.md`，添加“WindPath（原 QuickPath）”品牌过渡说明
- [x] 7. 全量编译验证与清理
  - [x] 7.1 运行全局残留扫描，确保无未处理的旧名称
  - [x] 7.2 运行 `cargo check` 和 `cargo test` 确保所有 254+ 单元测试 100% 通过
  - [x] 7.3 清理临时生成的探测排查文件
