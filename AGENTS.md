# super-mic

Rust daemon that colours a Winry315 keypad (USB `f1f1:0315`, QMK firmware) by microphone state on Linux (PipeWire/PulseAudio, Wayland). Dim golden yellow = no app using a mic, green = in use but all muted, red = any in-use mic unmuted. See `README.md` for user docs.

## Layout

- `src/main.rs` – event loop: query state, map to colour, apply (re-apply every 5 s and on reconnect).
- `src/mic.rs` – microphone state via `pactl` (`list sources` / `list source-outputs` as JSON, `pactl subscribe` for events).
- `src/keypad.rs` – finds the keypad's raw-HID `/dev/hidraw*` node via sysfs and sends VIA lighting commands.
- `src/config.rs` – YAML config (`~/.config/super-mic/config.yaml`), key-name parsing, defaults.
- `src/hotkey.rs` – pure hotkey logic (`Matcher`: hold-back/suppress state machine, active hold override, active hold override; time is injected) + `DeviceMatch`. Unit-tested; keep I/O out of it.
- `src/input.rs` – evdev: per-device threads, exclusive grab + uinput re-emit, hotplug rescan every 3 s, aggregates per-device requests (mute beats unmute).
- `src/mic.rs` also has the mute controller: applies hold-overrides to all non-monitor sources and restores the saved states on release.
- `contrib/` – systemd unit, udev rule + modules-load for uinput, `config.example.yaml`.
- `contrib/super-mic.service` – systemd **user** unit (installed to `~/.config/systemd/user/`).

## Commands

```bash
cargo build --release
cargo fmt --check && cargo clippy --all-targets -- -D warnings   # CI runs these
cargo test
cargo run --release                                              # logs to stderr
```

Installed service: `cargo install --path . && systemctl --user restart super-mic`; logs via `journalctl --user -u super-mic -f`.

## Hardware protocol facts (verified on the real device)

- Raw HID interface: usage page `0xFF60`, 32-byte reports, report id 0 (write 33 bytes incl. leading 0 to hidraw).
- VIA protocol 9 → **legacy lighting ids, no channel byte**: `[0x07, 0x80, brightness]`, `[0x07, 0x81, effect]` (1 = solid), `[0x07, 0x83, hue, sat]`. The v3 channel form (`[0x07, 3, id, ...]`) is silently ignored by this firmware.
- Hue/sat are 0–255 (golden yellow 30, green 85, red 0). Firmware caps brightness at 150.
- Never send VIA "save" (`0x09`) — colours must not be persisted to EEPROM.
- Replies must be read after each write or the hidraw buffer fills.

## Conventions

- A missing keypad is **not an error**: log only on connect/disconnect transitions, retry quietly. Don't add per-tick logging.
- Keep dependencies minimal (`serde_json`, `serde`, `serde_norway` for YAML, `evdev`); prefer std over new crates. (`serde_yaml` is deprecated, hence `serde_norway`.)
- Monitor sources (`*.monitor`), paused streams and level meters (pavucontrol) don't count as a mic in use.
- Hotkeys: actions are `mute`/`unmute`/`toggle`, all *holds* that restore each mic's previous state on release (`toggle` = invert the state of the mic(s) apps are *using*, not all mics: in-use live -> mute all, in-use muted -> unmute all; falls back to all mics if none in use). Keys of any shortcut are held back up to `HOLD_BACK` (150 ms) until the chord completes (discard) or is abandoned (flush in order); a timer thread per grabbed device flushes expired keys. Devices are grabbed only if a shortcut applies (device filter matches **and** it *advertises* all the keys; devices often advertise keys they lack, so this is not a reliable "has the key" test). Never grab a device whose name starts with `super-mic virtual` (our own uinput devices).
- Overrides: the longest matching hold shortcut wins and stays latched until no shortcut matches (release order must not flip the mics). The mute controller delays *unmute*/*toggle* by 60 ms so a sloppy chord can only err towards muted.
- Hotkey config errors disable hotkeys only; the colour feature must keep working.
- The grab/uinput path needs `input` group + `/dev/uinput` access and can't run in CI; test logic via `hotkey.rs` unit tests.
- Don't commit unless asked.
