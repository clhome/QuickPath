#![allow(dead_code)]

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};

/// 解析快捷键字符串（如 "Ctrl+Q", "Ctrl+Alt+Space", "F4"）
/// 返回 (HOT_KEY_MODIFIERS, vk_code)
pub fn parse_hotkey(hotkey_str: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let parts: Vec<&str> = hotkey_str
        .split(|c| c == '+' || c == '-' || c == ' ')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if parts.is_empty() {
        return None;
    }

    let mut modifiers = MOD_NOREPEAT.0;
    let mut key_code = None;

    for part in parts {
        let upper = part.to_ascii_uppercase();
        match upper.as_str() {
            "CTRL" | "CONTROL" => modifiers |= MOD_CONTROL.0,
            "ALT" => modifiers |= MOD_ALT.0,
            "SHIFT" => modifiers |= MOD_SHIFT.0,
            "WIN" | "WINDOWS" => modifiers |= MOD_WIN.0,
            "SPACE" => key_code = Some(0x20),
            "TAB" => key_code = Some(0x09),
            "ENTER" | "RETURN" => key_code = Some(0x0D),
            "`" | "~" => key_code = Some(0xC0),
            _ => {
                // F1 - F12
                if upper.starts_with('F') && upper.len() >= 2 {
                    if let Ok(num) = upper[1..].parse::<u32>() {
                        if (1..=12).contains(&num) {
                            key_code = Some(0x70 + (num - 1));
                            continue;
                        }
                    }
                }
                // 单字符 A-Z 或 0-9
                if upper.len() == 1 {
                    let ch = upper.chars().next().unwrap();
                    if ch.is_ascii_alphanumeric() {
                        key_code = Some(ch as u32);
                        continue;
                    }
                }
            }
        }
    }

    key_code.map(|vk| (HOT_KEY_MODIFIERS(modifiers), vk))
}

/// 格式化快捷键展示字符串，如将 "ctrl+q" 标准化为 "Ctrl + Q"
pub fn format_hotkey_display(hotkey_str: &str) -> String {
    let parts: Vec<&str> = hotkey_str
        .split(|c| c == '+' || c == '-' || c == ' ')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if parts.is_empty() {
        return "Ctrl + Q".to_string();
    }

    let mut result_parts = Vec::new();
    let mut main_key = String::new();

    for part in parts {
        let upper = part.to_ascii_uppercase();
        match upper.as_str() {
            "CTRL" | "CONTROL" => {
                if !result_parts.contains(&"Ctrl".to_string()) {
                    result_parts.push("Ctrl".to_string());
                }
            }
            "ALT" => {
                if !result_parts.contains(&"Alt".to_string()) {
                    result_parts.push("Alt".to_string());
                }
            }
            "SHIFT" => {
                if !result_parts.contains(&"Shift".to_string()) {
                    result_parts.push("Shift".to_string());
                }
            }
            "WIN" | "WINDOWS" => {
                if !result_parts.contains(&"Win".to_string()) {
                    result_parts.push("Win".to_string());
                }
            }
            _ => {
                main_key = upper;
            }
        }
    }

    if !main_key.is_empty() {
        result_parts.push(main_key);
    } else if result_parts.is_empty() {
        return "Ctrl + Q".to_string();
    }

    result_parts.join(" + ")
}

/// 从键盘事件虚拟键值转换为主键名称与是否为修饰键
pub fn vk_to_key_name(vk: u32) -> Option<&'static str> {
    match vk {
        0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0 | 0xA1 | 0xA2 | 0xA3 | 0xA4 | 0xA5 => None, // 修饰键本身不作为主键
        0x1B => Some("ESCAPE"),
        0x20 => Some("Space"),
        0x09 => Some("Tab"),
        0x0D => Some("Enter"),
        0xC0 => Some("`"),
        0x70 => Some("F1"),
        0x71 => Some("F2"),
        0x72 => Some("F3"),
        0x73 => Some("F4"),
        0x74 => Some("F5"),
        0x75 => Some("F6"),
        0x76 => Some("F7"),
        0x77 => Some("F8"),
        0x78 => Some("F9"),
        0x79 => Some("F10"),
        0x7A => Some("F11"),
        0x7B => Some("F12"),
        0x41 => Some("A"),
        0x42 => Some("B"),
        0x43 => Some("C"),
        0x44 => Some("D"),
        0x45 => Some("E"),
        0x46 => Some("F"),
        0x47 => Some("G"),
        0x48 => Some("H"),
        0x49 => Some("I"),
        0x4A => Some("J"),
        0x4B => Some("K"),
        0x4C => Some("L"),
        0x4D => Some("M"),
        0x4E => Some("N"),
        0x4F => Some("O"),
        0x50 => Some("P"),
        0x51 => Some("Q"),
        0x52 => Some("R"),
        0x53 => Some("S"),
        0x54 => Some("T"),
        0x55 => Some("U"),
        0x56 => Some("V"),
        0x57 => Some("W"),
        0x58 => Some("X"),
        0x59 => Some("Y"),
        0x5A => Some("Z"),
        0x30 => Some("0"),
        0x31 => Some("1"),
        0x32 => Some("2"),
        0x33 => Some("3"),
        0x34 => Some("4"),
        0x35 => Some("5"),
        0x36 => Some("6"),
        0x37 => Some("7"),
        0x38 => Some("8"),
        0x39 => Some("9"),
        _ => None,
    }
}

/// 动态注册全局热键（注销旧的，注册新的）
pub fn register_global_hotkey(hwnd: HWND, id: i32, hotkey_str: &str) -> bool {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), id);
        if let Some((modifiers, vk)) = parse_hotkey(hotkey_str) {
            RegisterHotKey(Some(hwnd), id, modifiers, vk).is_ok()
        } else {
            false
        }
    }
}
