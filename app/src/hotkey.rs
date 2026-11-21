//! Global shortcut parsing: "Ctrl+Alt+B", "Win+Shift+F9", "Ctrl+Alt+Num0"…
//! into RegisterHotKey modifiers and a virtual-key code.

pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;
pub const MOD_NOREPEAT: u32 = 0x4000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hotkey {
    pub modifiers: u32,
    pub vk: u32,
}

/// Returns `None` for an empty or invalid shortcut. At least one modifier
/// other than Shift is required, except for function keys.
pub fn parse(text: &str) -> Option<Hotkey> {
    let mut modifiers = 0;
    let mut vk = None;
    for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let p = part.to_ascii_lowercase();
        match p.as_str() {
            "ctrl" | "control" | "strg" => modifiers |= MOD_CONTROL,
            "alt" => modifiers |= MOD_ALT,
            "shift" | "maj" => modifiers |= MOD_SHIFT,
            "win" | "windows" | "super" => modifiers |= MOD_WIN,
            _ => {
                if vk.is_some() {
                    return None;
                }
                vk = Some(key(&p)?);
            }
        }
    }
    let vk = vk?;
    let function_key = (0x70..=0x87).contains(&vk);
    if modifiers & !MOD_SHIFT == 0 && !function_key {
        return None;
    }
    Some(Hotkey { modifiers: modifiers | MOD_NOREPEAT, vk })
}

fn key(p: &str) -> Option<u32> {
    let bytes = p.as_bytes();
    if bytes.len() == 1 && (bytes[0].is_ascii_alphanumeric()) {
        return Some(bytes[0].to_ascii_uppercase() as u32);
    }
    if let Some(n) = p.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then_some(0x6F + n);
    }
    if let Some(n) = p.strip_prefix("num").and_then(|n| n.parse::<u32>().ok()) {
        return (n <= 9).then_some(0x60 + n);
    }
    Some(match p {
        "space" | "espace" => 0x20,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "end" | "fin" => 0x23,
        "home" => 0x24,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "insert" | "ins" => 0x2D,
        "delete" | "del" | "suppr" => 0x2E,
        "pause" => 0x13,
        "scrolllock" => 0x91,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts() {
        assert_eq!(parse("Ctrl+Alt+B"), Some(Hotkey { modifiers: MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, vk: 'B' as u32 }));
        assert_eq!(parse(" win + shift + f9 "), Some(Hotkey { modifiers: MOD_WIN | MOD_SHIFT | MOD_NOREPEAT, vk: 0x78 }));
        assert_eq!(parse("F12").map(|h| h.vk), Some(0x7B));
        assert_eq!(parse("Ctrl+Num5").map(|h| h.vk), Some(0x65));
        assert_eq!(parse("Strg+Alt+7").map(|h| h.vk), Some('7' as u32));
        assert_eq!(parse(""), None);
        assert_eq!(parse("B"), None, "a plain letter would steal typing");
        assert_eq!(parse("Shift+B"), None);
        assert_eq!(parse("Ctrl+A+B"), None);
        assert_eq!(parse("Ctrl+F25"), None);
        assert_eq!(parse("Ctrl+Alt"), None);
    }
}
