use std::env;
use std::fs;

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=quickpath.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/logo.ico");

    // 1. 动态获取 Cargo.toml 中的版本号并规范为四段式
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.0.0".to_string());
    let mut parts: Vec<&str> = version.split('.').collect();
    while parts.len() < 4 {
        parts.push("0");
    }
    let comma_version = parts[..4].join(",");
    let dot_version = parts[..4].join(".");

    // 2. 同步 app.manifest 中的 assemblyIdentity 版本号
    if let Ok(manifest_content) = fs::read_to_string("app.manifest") {
        let mut new_lines = Vec::new();
        let mut modified = false;
        for line in manifest_content.lines() {
            if line.trim_start().starts_with("version=") {
                let leading_spaces: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                let target_line = format!("{}version=\"{}\"", leading_spaces, dot_version);
                if line != target_line {
                    new_lines.push(target_line);
                    modified = true;
                } else {
                    new_lines.push(line.to_string());
                }
            } else {
                new_lines.push(line.to_string());
            }
        }
        if modified {
            let new_content = new_lines.join("\n") + "\n";
            let _ = fs::write("app.manifest", new_content);
        }
    }

    // 3. 动态生成包含中英双语与完整软件元数据的 quickpath.rc
    let rc_content = format!(
r#"#pragma code_page(65001)

#define RT_MANIFEST 24
1 RT_MANIFEST "app.manifest"
1 ICON "assets/logo.ico"

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

    // 比对现有内容，避免无谓覆写触发构建循环
    let should_update_rc = match fs::read_to_string("quickpath.rc") {
        Ok(existing) => existing != rc_content,
        Err(_) => true,
    };

    if should_update_rc {
        let _ = fs::write("quickpath.rc", &rc_content);
    }

    let _ = embed_resource::compile("quickpath.rc", embed_resource::NONE);
}

