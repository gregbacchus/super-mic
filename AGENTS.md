# super-mic

Rust daemon that colours a Winry315 keypad (USB `f1f1:0315`, QMK firmware) by microphone state on Linux (PipeWire/PulseAudio, Wayland). Dim golden yellow = no app using a mic, green = in use but all muted, red = any in-use mic unmuted. See `README.md` for user docs.

## Layout

- `src/main.rs` – event loop: query state, map to colour, apply (re-apply every 5 s and on reconnect).
- `src/mic.rs` – microphone state via `pactl` (`list sources` / `list source-outputs` as JSON, `pactl subscribe` for events).
- `src/keypad.rs` – finds the keypad's raw-HID `/dev/hidraw*` node via sysfs and sends VIA lighting commands.
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
- Keep dependencies minimal (currently only `serde_json`); prefer std over new crates.
- Monitor sources (`*.monitor`), paused streams and level meters (pavucontrol) don't count as a mic in use.
- Don't commit unless asked.
