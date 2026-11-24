//! Hotkey matching: pure logic (no I/O, time is passed in) so it can be unit-tested.
//!
//! A shortcut is a set of keys that must all be held, pressed in any order.
//! To keep the whole combination invisible to other apps, presses of keys that
//! belong to some shortcut are briefly *held back* until it's clear whether
//! they form a chord:
//!
//! * chord completes   -> held-back presses (and their releases) are discarded
//! * another key comes -> held-back presses are released, in order, first
//! * `HOLD_BACK` passes -> held-back presses are released (slow chord: the
//!   modifiers leak, but the shortcut still works and the trigger is hidden)

use evdev::KeyCode;
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// How long a possible chord key is held back before being passed on.
pub const HOLD_BACK: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Mute all microphones while held.
    Mute,
    /// Unmute all microphones while held.
    Unmute,
    /// Invert the mute state while held: muted mics unmute, live mics mute.
    Toggle,
}

/// A temporary override while a shortcut is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    Mute,
    Unmute,
    /// Invert the current state.
    Toggle,
}

#[derive(Clone, Debug)]
pub struct Shortcut {
    pub action: Action,
    pub keys: Vec<KeyCode>,
    pub devices: Vec<DeviceMatch>,
}

impl Shortcut {
    fn hold(&self) -> Option<Hold> {
        match self.action {
            Action::Mute => Some(Hold::Mute),
            Action::Unmute => Some(Hold::Unmute),
            Action::Toggle => Some(Hold::Toggle),
        }
    }
}

/// Matches a physical input device. All given fields must match.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceMatch {
    /// Case-insensitive substring of the device name.
    pub name: Option<String>,
    pub vendor: Option<u16>,
    pub product: Option<u16>,
}

impl DeviceMatch {
    pub fn matches(&self, name: &str, vendor: u16, product: u16) -> bool {
        self.name
            .as_ref()
            .is_none_or(|n| name.to_lowercase().contains(&n.to_lowercase()))
            && self.vendor.is_none_or(|v| v == vendor)
            && self.product.is_none_or(|p| p == product)
    }
}

/// A key event to pass on to other apps (`value`: 1 press, 0 release, 2 repeat).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyOut {
    pub code: u16,
    pub value: i32,
}

/// Per-device key state machine.
pub struct Matcher {
    shortcuts: Vec<Shortcut>,
    /// Keys physically down.
    held: HashSet<u16>,
    /// Presses held back, oldest first.
    buffered: Vec<(u16, Instant)>,
    /// Keys whose press was discarded (their release/repeats are too).
    suppressed: HashSet<u16>,
    /// Hold override in effect, and the length of the shortcut that set it.
    latched: Option<(Hold, usize)>,
}

impl Matcher {
    pub fn new(shortcuts: Vec<Shortcut>, already_held: impl IntoIterator<Item = u16>) -> Self {
        Matcher {
            shortcuts,
            held: already_held.into_iter().collect(),
            buffered: Vec::new(),
            suppressed: HashSet::new(),
            latched: None,
        }
    }

    /// Feed a key event; returns the key events to pass on, in order.
    pub fn on_key(&mut self, code: u16, value: i32, now: Instant) -> Vec<KeyOut> {
        let out = match value {
            1 => self.press(code, now),
            0 => self.release(code),
            _ if self.suppressed.contains(&code) || self.is_buffered(code) => vec![],
            _ => vec![KeyOut { code, value }],
        };
        self.refresh();
        out
    }

    /// Call periodically: releases held-back presses that have waited too long.
    pub fn tick(&mut self, now: Instant) -> Vec<KeyOut> {
        match self.buffered.first() {
            Some((_, t)) if now.saturating_duration_since(*t) >= HOLD_BACK => self.flush(),
            _ => vec![],
        }
    }

    /// The hold override currently requested.
    pub fn active(&self) -> Option<Hold> {
        self.latched.map(|(h, _)| h)
    }

    fn press(&mut self, code: u16, now: Instant) -> Vec<KeyOut> {
        self.held.insert(code);
        if self.is_member(code) {
            if self.complete() {
                for (c, _) in self.buffered.drain(..) {
                    self.suppressed.insert(c);
                }
                self.suppressed.insert(code);
                return vec![];
            }
            if self.could_still_complete() {
                self.buffered.push((code, now));
                return vec![];
            }
        }
        let mut out = self.flush();
        out.push(KeyOut { code, value: 1 });
        out
    }

    fn release(&mut self, code: u16) -> Vec<KeyOut> {
        self.held.remove(&code);
        if self.suppressed.remove(&code) {
            return vec![];
        }
        let mut out = if self.is_buffered(code) {
            self.flush()
        } else {
            vec![]
        };
        out.push(KeyOut { code, value: 0 });
        out
    }

    fn flush(&mut self) -> Vec<KeyOut> {
        self.buffered
            .drain(..)
            .map(|(code, _)| KeyOut { code, value: 1 })
            .collect()
    }

    fn is_buffered(&self, code: u16) -> bool {
        self.buffered.iter().any(|(c, _)| *c == code)
    }

    fn is_member(&self, code: u16) -> bool {
        self.shortcuts
            .iter()
            .any(|s| s.keys.iter().any(|k| k.code() == code))
    }

    fn all_held(&self, s: &Shortcut) -> bool {
        s.keys.iter().all(|k| self.held.contains(&k.code()))
    }

    fn complete(&self) -> bool {
        self.shortcuts.iter().any(|s| self.all_held(s))
    }

    /// Everything held so far fits inside some shortcut (so more keys may follow).
    fn could_still_complete(&self) -> bool {
        self.shortcuts.iter().any(|s| {
            self.held
                .iter()
                .all(|h| s.keys.iter().any(|k| k.code() == *h))
        })
    }

    /// Updates the hold override.
    ///
    /// The matching hold shortcut with the most keys wins (mute wins ties), and
    /// once an override is in effect a shorter match can't replace it: releasing
    /// the keys in a different order (e.g. Shift just before Menu) must not
    /// flip the mics for an instant.
    fn refresh(&mut self) {
        let best = self
            .shortcuts
            .iter()
            .filter(|s| self.all_held(s))
            .filter_map(|s| Some((s.hold()?, s.keys.len())))
            .max_by_key(|(h, len)| (*len, *h == Hold::Mute));
        self.latched = match (best, self.latched) {
            (None, _) => None,
            (Some((_, len)), Some((h, latched_len))) if latched_len >= len => {
                Some((h, latched_len))
            }
            (Some(b), _) => Some(b),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: KeyCode = KeyCode::KEY_RIGHTCTRL;
    const SHIFT: KeyCode = KeyCode::KEY_RIGHTSHIFT;
    const MENU: KeyCode = KeyCode::KEY_COMPOSE;
    const A: KeyCode = KeyCode::KEY_A;

    fn sc(action: Action, keys: &[KeyCode]) -> Shortcut {
        Shortcut {
            action,
            keys: keys.to_vec(),
            devices: vec![],
        }
    }

    /// Defaults: ctrl+menu = mute, ctrl+shift+menu = unmute.
    fn matcher() -> Matcher {
        Matcher::new(
            vec![
                sc(Action::Mute, &[CTRL, MENU]),
                sc(Action::Unmute, &[CTRL, SHIFT, MENU]),
            ],
            [],
        )
    }

    fn press(k: KeyCode) -> KeyOut {
        KeyOut {
            code: k.code(),
            value: 1,
        }
    }
    fn release(k: KeyCode) -> KeyOut {
        KeyOut {
            code: k.code(),
            value: 0,
        }
    }

    /// Plays key events at the same instant, returning everything passed on.
    fn play(m: &mut Matcher, events: &[(KeyCode, i32)]) -> Vec<KeyOut> {
        let now = Instant::now();
        events
            .iter()
            .flat_map(|(k, v)| m.on_key(k.code(), *v, now))
            .collect()
    }

    #[test]
    fn whole_chord_is_hidden() {
        let mut m = matcher();
        let out = play(&mut m, &[(CTRL, 1), (MENU, 1)]);
        assert!(out.is_empty());
        assert_eq!(m.active(), Some(Hold::Mute));
        assert!(play(&mut m, &[(MENU, 2), (MENU, 0), (CTRL, 0)]).is_empty());
        assert_eq!(m.active(), None);
    }

    #[test]
    fn unmute_chord_is_hidden_and_overrides_mute() {
        let mut m = matcher();
        let out = play(&mut m, &[(CTRL, 1), (SHIFT, 1), (MENU, 1)]);
        assert!(out.is_empty());
        assert_eq!(m.active(), Some(Hold::Unmute));
    }

    #[test]
    fn adding_shift_upgrades_mute_to_unmute() {
        let mut m = matcher();
        play(&mut m, &[(CTRL, 1), (MENU, 1)]);
        assert_eq!(m.active(), Some(Hold::Mute));
        assert!(play(&mut m, &[(SHIFT, 1)]).is_empty());
        assert_eq!(m.active(), Some(Hold::Unmute));
    }

    #[test]
    fn longer_override_stays_latched_when_shift_released_first() {
        let mut m = matcher();
        play(&mut m, &[(CTRL, 1), (SHIFT, 1), (MENU, 1)]);
        assert_eq!(m.active(), Some(Hold::Unmute));
        play(&mut m, &[(SHIFT, 0)]); // ctrl+menu still matches "mute"...
        assert_eq!(m.active(), Some(Hold::Unmute)); // ...but unmute is latched
        play(&mut m, &[(MENU, 0)]);
        assert_eq!(m.active(), None);
        play(&mut m, &[(CTRL, 0)]);
    }

    #[test]
    fn works_in_any_order() {
        let mut m = matcher();
        assert!(play(&mut m, &[(MENU, 1), (SHIFT, 1), (CTRL, 1)]).is_empty());
        assert_eq!(m.active(), Some(Hold::Unmute));
        assert!(play(&mut m, &[(CTRL, 0), (SHIFT, 0), (MENU, 0)]).is_empty());
        assert_eq!(m.active(), None);
    }

    #[test]
    fn lone_tap_passes_through() {
        let mut m = matcher();
        let out = play(&mut m, &[(MENU, 1), (MENU, 0)]);
        assert_eq!(out, vec![press(MENU), release(MENU)]);
    }

    #[test]
    fn held_back_key_is_released_after_timeout() {
        let mut m = matcher();
        let t = Instant::now();
        assert!(m.on_key(CTRL.code(), 1, t).is_empty());
        assert!(m.tick(t + Duration::from_millis(50)).is_empty());
        assert_eq!(m.tick(t + HOLD_BACK), vec![press(CTRL)]);
        // Slow chord: Ctrl already passed on, but the shortcut still works
        // and its trigger is hidden.
        assert!(m.on_key(MENU.code(), 1, t + HOLD_BACK).is_empty());
        assert_eq!(m.active(), Some(Hold::Mute));
        assert_eq!(m.on_key(CTRL.code(), 0, t + HOLD_BACK), vec![release(CTRL)]);
    }

    #[test]
    fn other_key_releases_held_back_keys_in_order() {
        let mut m = matcher();
        let out = play(&mut m, &[(SHIFT, 1), (A, 1)]); // e.g. typing a capital
        assert_eq!(out, vec![press(SHIFT), press(A)]);
        assert_eq!(
            play(&mut m, &[(A, 0), (SHIFT, 0)]),
            vec![release(A), release(SHIFT)]
        );
    }

    #[test]
    fn modifier_after_other_key_is_not_held_back() {
        let mut m = matcher();
        assert_eq!(play(&mut m, &[(A, 1)]), vec![press(A)]);
        assert_eq!(play(&mut m, &[(CTRL, 1)]), vec![press(CTRL)]); // ctrl+a style use
    }

    #[test]
    fn keys_already_held_at_start_count() {
        let mut m = Matcher::new(vec![sc(Action::Mute, &[CTRL, MENU])], [CTRL.code()]);
        assert!(play(&mut m, &[(MENU, 1)]).is_empty());
        assert_eq!(m.active(), Some(Hold::Mute));
        assert_eq!(play(&mut m, &[(CTRL, 0)]), vec![release(CTRL)]); // press was before us
    }

    #[test]
    fn single_key_shortcut() {
        let mut m = Matcher::new(vec![sc(Action::Unmute, &[KeyCode::KEY_F13])], []);
        assert!(play(&mut m, &[(KeyCode::KEY_F13, 1)]).is_empty());
        assert_eq!(m.active(), Some(Hold::Unmute));
    }

    #[test]
    fn toggle_is_a_hidden_hold() {
        let mut m = Matcher::new(vec![sc(Action::Toggle, &[CTRL, MENU])], []);
        assert!(play(&mut m, &[(CTRL, 1), (MENU, 1)]).is_empty());
        assert_eq!(m.active(), Some(Hold::Toggle));
        assert!(play(&mut m, &[(MENU, 2), (MENU, 0), (CTRL, 0)]).is_empty());
        assert_eq!(m.active(), None);
    }

    #[test]
    fn device_match_rules() {
        let m = DeviceMatch {
            name: Some("keyboard".into()),
            vendor: None,
            product: None,
        };
        assert!(m.matches("Microsoft Wired Keyboard 600", 1, 2));
        assert!(!m.matches("dztech YD3xn15mx", 1, 2));
        let m = DeviceMatch {
            name: None,
            vendor: Some(0xf1f1),
            product: Some(0x315),
        };
        assert!(m.matches("anything", 0xf1f1, 0x315));
        assert!(!m.matches("anything", 0xf1f1, 0x316));
    }
}
