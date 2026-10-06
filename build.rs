use std::env;
use std::fs;

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=quickpath.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/logo.ico");

    // 动态同步 app.manifest 中的 assemblyIdentity 4 段式版本号
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.0.0".to_string());
    let mut parts: Vec<&str> = version.split('.').collect();
    while parts.len() < 4 {
        parts.push("0");
    }
    let manifest_version = parts[..4].join(".");

    if let Ok(manifest_content) = fs::read_to_string("app.manifest") {
        let mut new_lines = Vec::new();
        let mut modified = false;
        for line in manifest_content.lines() {
            if line.trim_start().starts_with("version=") {
                let leading_spaces: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                let target_line = format!("{}version=\"{}\"", leading_spaces, manifest_version);
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

    let _ = embed_resource::compile("quickpath.rc", embed_resource::NONE);
}
