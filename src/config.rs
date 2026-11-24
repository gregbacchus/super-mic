//! YAML configuration (`$XDG_CONFIG_HOME/super-mic/config.yaml`).

use crate::hotkey::{Action, DeviceMatch, Shortcut};
use evdev::KeyCode;
use serde::Deserialize;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    shortcuts: Option<Vec<RawShortcut>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawShortcut {
    action: Action,
    keys: Vec<String>,
    #[serde(default)]
    devices: Vec<DeviceMatch>,
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("super-mic").join("config.yaml")
}

/// Loads shortcuts. A missing file means defaults; a broken file is an error.
pub fn load() -> Result<Vec<Shortcut>, String> {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("no config at {}, using default shortcuts", path.display());
            Ok(default_shortcuts())
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn parse(text: &str) -> Result<Vec<Shortcut>, String> {
    let raw: RawConfig = serde_norway::from_str(text).map_err(|e| e.to_string())?;
    let Some(raw) = raw.shortcuts else {
        return Ok(default_shortcuts());
    };
    raw.into_iter()
        .enumerate()
        .map(|(i, s)| convert(s).map_err(|e| format!("shortcut #{}: {e}", i + 1)))
        .collect()
}

fn convert(s: RawShortcut) -> Result<Shortcut, String> {
    if s.keys.is_empty() {
        return Err("`keys` must not be empty".into());
    }
    if s.devices
        .iter()
        .any(|d| d.name.is_none() && d.vendor.is_none() && d.product.is_none())
    {
        return Err("a `devices` entry needs at least one of name, vendor, product".into());
    }
    let keys = s
        .keys
        .iter()
        .map(|k| parse_key(k))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Shortcut {
        action: s.action,
        keys,
        devices: s.devices,
    })
}

/// Right Ctrl + Menu = mute while held; Right Ctrl + Right Shift + Menu = unmute.
pub fn default_shortcuts() -> Vec<Shortcut> {
    let k = KeyCode::KEY_RIGHTCTRL;
    vec![
        Shortcut {
            action: Action::Mute,
            keys: vec![k, KeyCode::KEY_COMPOSE],
            devices: vec![],
        },
        Shortcut {
            action: Action::Unmute,
            keys: vec![k, KeyCode::KEY_RIGHTSHIFT, KeyCode::KEY_COMPOSE],
            devices: vec![],
        },
    ]
}

/// Accepts evdev names (`KEY_COMPOSE`) or short forms (`menu`, `r_ctrl`, `f13`, `a`).
pub fn parse_key(name: &str) -> Result<KeyCode, String> {
    let u = name.trim().to_uppercase().replace(['-', ' '], "_");
    let canonical = match u.as_str() {
        "MENU" | "COMPOSE" => "KEY_COMPOSE".to_string(),
        "R_CTRL" | "RCTRL" | "RIGHT_CTRL" => "KEY_RIGHTCTRL".to_string(),
        "L_CTRL" | "LCTRL" | "LEFT_CTRL" => "KEY_LEFTCTRL".to_string(),
        "R_SHIFT" | "RSHIFT" | "RIGHT_SHIFT" => "KEY_RIGHTSHIFT".to_string(),
        "L_SHIFT" | "LSHIFT" | "LEFT_SHIFT" => "KEY_LEFTSHIFT".to_string(),
        "R_ALT" | "RALT" | "RIGHT_ALT" | "ALTGR" => "KEY_RIGHTALT".to_string(),
        "L_ALT" | "LALT" | "LEFT_ALT" => "KEY_LEFTALT".to_string(),
        "R_META" | "RMETA" | "R_SUPER" | "RIGHT_META" => "KEY_RIGHTMETA".to_string(),
        "L_META" | "LMETA" | "L_SUPER" | "LEFT_META" | "SUPER" => "KEY_LEFTMETA".to_string(),
        "CTRL" | "SHIFT" | "ALT" => {
            return Err(format!(
                "`{name}` is ambiguous, use the left/right form (e.g. L_{u}, R_{u})"
            ));
        }
        _ if u.starts_with("KEY_") || u.starts_with("BTN_") => u.clone(),
        _ => format!("KEY_{u}"),
    };
    KeyCode::from_str(&canonical).map_err(|_| format!("unknown key `{name}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_no_shortcuts_key() {
        let s = parse("{}").unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].action, Action::Mute);
        assert_eq!(s[0].keys.len(), 2);
        assert_eq!(s[1].action, Action::Unmute);
        assert_eq!(s[1].keys.len(), 3);
    }

    #[test]
    fn parses_per_device_shortcuts() {
        let s = parse(
            "
shortcuts:
  - action: mute
    keys: [R_CTRL, MENU]
  - action: toggle
    keys: [KEY_F13]
    devices:
      - name: Keyboard 600
      - vendor: 0xf1f1
        product: 0x0315
",
        )
        .unwrap();
        assert_eq!(s.len(), 2);
        assert!(s[0].devices.is_empty());
        assert_eq!(s[1].devices.len(), 2);
        assert_eq!(s[1].devices[1].vendor, Some(0xf1f1));
        assert_eq!(s[1].action, Action::Toggle);
        assert_eq!(s[1].keys, vec![KeyCode::KEY_F13]);
    }

    #[test]
    fn key_name_forms() {
        assert_eq!(parse_key("menu").unwrap(), KeyCode::KEY_COMPOSE);
        assert_eq!(parse_key("r-ctrl").unwrap(), KeyCode::KEY_RIGHTCTRL);
        assert_eq!(parse_key("f13").unwrap(), KeyCode::KEY_F13);
        assert_eq!(parse_key("a").unwrap(), KeyCode::KEY_A);
        assert_eq!(
            parse_key("KEY_SCROLLLOCK").unwrap(),
            KeyCode::KEY_SCROLLLOCK
        );
        assert!(parse_key("ctrl").is_err());
        assert!(parse_key("nonsense").is_err());
    }

    #[test]
    fn rejects_bad_config() {
        assert!(parse("shortcuts: [{action: mute, keys: []}]").is_err());
        assert!(parse("shortcuts: [{action: flip, keys: [a]}]").is_err());
        assert!(parse("shortcuts: [{action: mute, keys: [a], devices: [{}]}]").is_err());
        assert!(parse("shortcuts: [{action: mute, keys: [a], extra: 1}]").is_err());
    }
}
