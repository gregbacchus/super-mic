# super-mic

Changes the backlight colour of a Winry315 keypad (`f1f1:0315`) to show microphone state on Linux (PipeWire / PulseAudio, works under Wayland).

| Colour | Meaning |
|--------|---------|
| Golden yellow (dim) | No app is using a microphone |
| Green  | App(s) are using microphone(s) and all of them are muted |
| Red    | An app is using a microphone that is **unmuted** |

All microphones are considered. "Using" means an app has an active, un-paused recording stream on a non-monitor source. Volume meters (e.g. pavucontrol) are ignored. A mic counts as muted if either the device or the app's stream is muted.

## Requirements

- `pactl` (provided by `pipewire-pulse` / `pulseaudio-utils`)
- Rust toolchain
- Permission to open the keypad's `/dev/hidraw*` node, e.g. `/etc/udev/rules.d/60-winry315.rules`:

  ```
  KERNEL=="hidraw*", ATTRS{idVendor}=="f1f1", ATTRS{idProduct}=="0315", TAG+="uaccess"
  ```

## Run

```bash
cargo run --release
```

## Install as a service (autostart on login)

```bash
cargo install --path .
mkdir -p ~/.config/systemd/user
cp contrib/super-mic.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now super-mic.service
```

After code changes: `cargo install --path . && systemctl --user restart super-mic`.
Logs: `journalctl --user -u super-mic -f`.

## Behaviour notes

- The keypad not being plugged in is not an error. A line is logged when it connects or disconnects, and the colour is reapplied as soon as it reappears.
- If `pactl` can't be found or the sound server isn't up yet, the error is logged once and retried; the service unit sets a `PATH` that includes `/usr/bin` and Homebrew (`/home/linuxbrew/.linuxbrew/bin`). Edit the unit if your `pactl` lives elsewhere.
- Colour is reapplied every 5 s, so replugging or a keypad reset recovers by itself.
- Colours are never saved to the keypad's EEPROM; replugging restores your saved lighting until the next update. Quitting the program leaves the last colour showing.

## How it works

- **Audio:** `pactl subscribe` triggers a re-query (`pactl -f json list sources` / `source-outputs`) when sources or recording streams change (`src/mic.rs`).
- **Keypad:** the QMK firmware speaks VIA protocol 9 on its raw HID interface (usage page `0xFF60`). Lighting uses the legacy VIA ids with no channel byte: `0x80` brightness, `0x81` effect (1 = solid), `0x83` hue + saturation (`src/keypad.rs`).
