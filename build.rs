use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/logo.ico");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("缺少 OUT_DIR 环境变量"));
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("缺少 CARGO_MANIFEST_DIR 环境变量"));

    // 1. 动态获取 Cargo.toml 中的版本号并规范为四段式
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.0.0".to_string());
    let mut parts: Vec<&str> = version.split('.').collect();
    while parts.len() < 4 {
        parts.push("0");
    }
    let comma_version = parts[..4].join(",");
    let dot_version = parts[..4].join(".");

    // 2. 把清单模板中的 assemblyIdentity 版本号改为当前版本，输出到 OUT_DIR。
    //
    // 审计报告 P2-03：构建过程**绝不改写源码树**——旧实现直接 `fs::write("app.manifest")`
    // 会弄脏工作区、在只读源码 / 并行工作树 / 打包环境下失败，并可能把旧资源编进产物。
    // 现在源目录只保留模板，所有生成物一律写入 OUT_DIR。
    let manifest_template = fs::read_to_string(manifest_dir.join("app.manifest"))
        .expect("读取 app.manifest 模板失败");
    let patched_manifest: String = manifest_template
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("version=") {
                let leading_spaces: String =
                    line.chars().take_while(|c| c.is_whitespace()).collect();
                format!("{}version=\"{}\"", leading_spaces, dot_version)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(out_dir.join("app.manifest"), patched_manifest).expect("写出清单到 OUT_DIR 失败");

    // 3. 图标也复制到 OUT_DIR，使资源脚本里的所有引用都是“同目录相对路径”，
    //    避免在 RC 字符串里写含反斜杠的 Windows 绝对路径（转义易错、跨环境不可移植）。
    fs::copy(
        manifest_dir.join("assets/logo.ico"),
        out_dir.join("logo.ico"),
    )
    .expect("复制 logo.ico 到 OUT_DIR 失败");

    // 4. 动态生成包含中英双语与完整软件元数据的资源脚本（写入 OUT_DIR）
    let rc_content = format!(
r#"#pragma code_page(65001)

#define RT_MANIFEST 24
1 RT_MANIFEST "app.manifest"
1 ICON "logo.ico"

1 VERSIONINFO
FILEVERSION {comma_version}
PRODUCTVERSION {comma_version}
FILEFLAGSMASK 0x3fL
#ifdef _DEBUG
 FILEFLAGS 0x1L
#else
 FILEFLAGS 0x0L
#endif
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "080404b0"
        BEGIN
            VALUE "CompanyName", "衢州御风科技有限公司"
            VALUE "FileDescription", "QuickPath - 现代文件对话框快速跳转增强工具"
            VALUE "FileVersion", "{dot_version}"
            VALUE "InternalName", "quickpath.exe"
            VALUE "LegalCopyright", "Copyright (C) 2026 衢州御风科技有限公司. 保留所有权利."
            VALUE "OriginalFilename", "quickpath.exe"
            VALUE "ProductName", "QuickPath"
            VALUE "ProductVersion", "{dot_version}"
            VALUE "Comments", "https://qp.yftec.top"
        END
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "Quzhou Yufeng Technology Co., Ltd."
            VALUE "FileDescription", "QuickPath - Modern File Dialog Quick Switcher"
            VALUE "FileVersion", "{dot_version}"
            VALUE "InternalName", "quickpath.exe"
            VALUE "LegalCopyright", "Copyright (C) 2026 Quzhou Yufeng Technology Co., Ltd. All rights reserved."
            VALUE "OriginalFilename", "quickpath.exe"
            VALUE "ProductName", "QuickPath"
            VALUE "ProductVersion", "{dot_version}"
            VALUE "Comments", "https://qp.yftec.top"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0804, 0x04b0, 0x0409, 0x04b0
    END
END
"#
    );

    let rc_path = out_dir.join("quickpath.rc");
    fs::write(&rc_path, rc_content).expect("写出资源脚本到 OUT_DIR 失败");

    // 5. 编译资源。失败必须让构建失败，绝不静默忽略（旧实现 `let _ =` 会产出无版本信息/无图标的 exe）。
    use embed_resource::CompilationResult;
    match embed_resource::compile(&rc_path, embed_resource::NONE) {
        CompilationResult::Ok | CompilationResult::NotWindows => {}
        CompilationResult::NotAttempted(msg) | CompilationResult::Failed(msg) => {
            panic!(
                "编译 Windows 资源失败（版本信息/图标将缺失），请检查资源编译器: {}",
                msg
            );
        }
    }
}
