# super-mic

Changes the backlight colour of a Winry315 keypad (`f1f1:0315`) to show microphone state on Linux (PipeWire / PulseAudio, works under Wayland), and adds hold-to-mute / hold-to-unmute global hotkeys.

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

## Hotkeys

Global shortcuts that act on **all microphones**:

| Action | Behaviour |
|--------|-----------|
| `mute` | Mutes while the combination is held; each mic returns to its previous state on release |
| `unmute` | Unmutes while held; same restore on release (push-to-talk) |
| `toggle` | Inverts the state of the mic(s) apps are using while held: if one was live it mutes all, if they were all muted it unmutes all (with no mic in use, it looks at all mics); restored on release |

Defaults (any keyboard that has a Menu key):

| Shortcut | Action |
|----------|--------|
| Right Ctrl + Menu | mute while held |
| Right Ctrl + Right Shift + Menu | unmute while held |

Keys can be pressed in any order. If several shortcuts match, the one with more keys wins (on equal length mute wins, then unmute, then toggle), and once an override is in effect it stays until the hold ends (so releasing Shift slightly before Menu doesn't flip the mics for an instant). The keypad colour follows the real mute state.

**Other apps don't see the combination.** A key that belongs to a shortcut is briefly held back (up to 150 ms) until it's clear whether it's a chord: if the chord completes, the keys are discarded; if you press anything else first (e.g. Shift+A) or release too early, they are passed on in order. So a lone tap of Menu still works, and a very slow chord may leak its modifier keys (the shortcut still works and the last key is hidden).

Shortcuts are configured in `~/.config/super-mic/config.yaml` (see [contrib/config.example.yaml](contrib/config.example.yaml)). You can define any number of shortcuts and restrict each to particular input devices by name, or USB vendor/product, so different keyboards can have different combinations:

```yaml
shortcuts:
  - action: mute
    keys: [r_ctrl, menu]
  - action: unmute
    keys: [r_ctrl, r_shift, menu]
  - action: toggle          # laptop keyboard has no Menu key
    keys: [r_ctrl, r_alt]
    devices:
      - name: AT Translated Set 2 keyboard
  - action: toggle          # a key on the external keypad
    keys: [f13]
    devices:
      - vendor: 0xf1f1
        product: 0x0315
```

A device is only used for a shortcut if it matches the `devices` filter (if any) **and** advertises all its keys. Devices with no applicable shortcut are never grabbed. Note many keyboards advertise keys they don't physically have (e.g. a laptop keyboard still reports a Menu key), so use `devices` to restrict a shortcut to the keyboards you want; find device names in the `hotkeys: grabbed …` log lines or `/proc/bus/input/devices`. Defining `shortcuts` replaces the defaults. A config error disables hotkeys (logged) but not the colours. Restart the service after editing.

Set `SUPER_MIC_DEBUG=1` in the service environment to log every key event on grabbed keyboards (this records what you type; remove it afterwards).

### One-time setup for hotkeys

Wayland has no global-hotkey API, so super-mic reads keyboards from `/dev/input` and, for devices with a shortcut, grabs them and forwards all other keys through a virtual keyboard (`/dev/uinput`). That needs:

```bash
sudo usermod -aG input "$USER"                                   # then log out and back in
sudo cp contrib/70-super-mic-uinput.rules /etc/udev/rules.d/
sudo cp contrib/uinput-modules.conf /etc/modules-load.d/uinput.conf
sudo modprobe uinput
sudo udevadm control --reload && sudo udevadm trigger --name-match=uinput
```

**Security note:** membership of the `input` group lets any program running as you read every keystroke on the machine (including passwords). Only do this on a machine you're comfortable with that on.

Caveats: caps/num-lock LEDs on a grabbed keyboard are not driven by the virtual device; if super-mic is killed while a hotkey is held, microphones stay in the overridden state until you change them.

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
