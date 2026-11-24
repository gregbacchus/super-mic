//! Global hotkeys on Wayland: read keyboards from /dev/input, grab those that
//! have a matching shortcut, and re-emit every event except the keys of a
//! completed shortcut through a virtual (uinput) keyboard.
//!
//! Needs read/write access to /dev/input/event* and /dev/uinput (see README).

use crate::hotkey::{Hold, Matcher, Shortcut};
use evdev::uinput::VirtualDevice;
use evdev::{Device, EventType, InputEvent};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const VIRTUAL_PREFIX: &str = "super-mic virtual";
/// `SUPER_MIC_DEBUG=1` logs every key event on grabbed devices.
fn debug() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SUPER_MIC_DEBUG").is_some())
}

const RESCAN_EVERY: Duration = Duration::from_secs(3);

/// Combines per-device hold requests (mute, then unmute, then invert) and emits on change.
struct Aggregate {
    wants: HashMap<u64, Hold>,
    last: Option<Hold>,
    tx: Sender<Option<Hold>>,
}

impl Aggregate {
    fn set(&mut self, id: u64, want: Option<Hold>) {
        match want {
            Some(h) => self.wants.insert(id, h),
            None => self.wants.remove(&id),
        };
        let values = || self.wants.values();
        // Most protective first: mute, then unmute, then invert.
        let agg = [Hold::Mute, Hold::Unmute, Hold::Toggle]
            .into_iter()
            .find(|want| values().any(|h| h == want));
        if agg != self.last {
            self.last = agg;
            let _ = self.tx.send(agg);
        }
    }
}

/// Starts the device scanner. Does nothing if there are no shortcuts.
pub fn spawn(shortcuts: Vec<Shortcut>, tx: Sender<Option<Hold>>) {
    if shortcuts.is_empty() {
        return;
    }
    let agg = Arc::new(Mutex::new(Aggregate {
        wants: HashMap::new(),
        last: None,
        tx,
    }));
    std::thread::spawn(move || scan_loop(shortcuts, agg));
}

fn scan_loop(shortcuts: Vec<Shortcut>, agg: Arc<Mutex<Aggregate>>) {
    let active: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
    let mut logged: HashSet<String> = HashSet::new(); // dedupe repeated problems
    let mut next_id = 0u64;
    let mut warn = |msg: String| {
        if logged.insert(msg.clone()) {
            eprintln!("hotkeys: {msg}");
        }
    };

    loop {
        let paths = std::fs::read_dir("/dev/input")
            .map(|d| d.flatten().map(|e| e.path()).collect::<Vec<_>>())
            .unwrap_or_default();
        for path in paths {
            let is_event = path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("event"));
            if !is_event || active.lock().unwrap().contains(&path) {
                continue;
            }
            let dev = match Device::open(&path) {
                Ok(d) => d,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    warn("no permission to read /dev/input (add your user to the `input` group, see README)".into());
                    continue;
                }
                Err(_) => continue, // vanished or not a usable device
            };
            let name = dev.name().unwrap_or("unknown").to_string();
            if name.starts_with(VIRTUAL_PREFIX) {
                continue;
            }
            let applicable = applicable_shortcuts(&shortcuts, &dev, &name);
            if applicable.is_empty() {
                continue;
            }
            match start_device(dev, &name, applicable) {
                Ok((dev, vdev, matcher)) => {
                    let id = next_id;
                    next_id += 1;
                    active.lock().unwrap().insert(path.clone());
                    eprintln!("hotkeys: grabbed {name} ({})", path.display());
                    let (agg, active, path) = (agg.clone(), active.clone(), path.clone());
                    std::thread::spawn(move || {
                        let end = run_device(dev, vdev, matcher, id, &agg);
                        agg.lock().unwrap().set(id, None);
                        active.lock().unwrap().remove(&path);
                        eprintln!("hotkeys: released {name} ({end})");
                    });
                }
                Err(e) => warn(format!("{name}: {e}")),
            }
        }
        std::thread::sleep(RESCAN_EVERY);
    }
}

/// Shortcuts for this device: its filter matches (or has none) and the device
/// actually has all the keys (so a laptop keyboard without Menu is left alone).
fn applicable_shortcuts(shortcuts: &[Shortcut], dev: &Device, name: &str) -> Vec<Shortcut> {
    let id = dev.input_id();
    let Some(keys) = dev.supported_keys() else {
        return vec![];
    };
    shortcuts
        .iter()
        .filter(|s| {
            (s.devices.is_empty()
                || s.devices
                    .iter()
                    .any(|m| m.matches(name, id.vendor(), id.product())))
                && s.keys.iter().all(|k| keys.contains(*k))
        })
        .cloned()
        .collect()
}

fn start_device(
    mut dev: Device,
    name: &str,
    shortcuts: Vec<Shortcut>,
) -> Result<(Device, VirtualDevice, Matcher), String> {
    if dev.supported_absolute_axes().is_some() {
        return Err("has absolute axes (not a plain keyboard), not grabbing".into());
    }
    let vname = format!("{VIRTUAL_PREFIX} ({name})");
    let mut builder = VirtualDevice::builder()
        .map_err(|e| format!("cannot open /dev/uinput: {e} (see README)"))?
        .name(&vname)
        .input_id(dev.input_id());
    let err = |e: std::io::Error| format!("creating virtual device: {e}");
    if let Some(keys) = dev.supported_keys() {
        builder = builder.with_keys(keys).map_err(err)?;
    }
    if let Some(rel) = dev.supported_relative_axes() {
        builder = builder.with_relative_axes(rel).map_err(err)?;
    }
    let vdev = builder.build().map_err(err)?;
    let held = dev
        .get_key_state()
        .map(|s| s.iter().map(|k| k.code()).collect::<Vec<_>>())
        .unwrap_or_default();
    dev.grab().map_err(|e| format!("grab failed: {e}"))?;
    Ok((dev, vdev, Matcher::new(shortcuts, held)))
}

/// Matcher plus the output device; shared by the reader and the hold-back timer.
struct Shared {
    matcher: Matcher,
    vdev: VirtualDevice,
}

fn key_events(outs: &[crate::hotkey::KeyOut]) -> Vec<InputEvent> {
    outs.iter()
        .map(|o| InputEvent::new(EventType::KEY.0, o.code, o.value))
        .collect()
}

/// Forwards events until the device goes away. Returns why it stopped.
fn run_device(
    mut dev: Device,
    vdev: VirtualDevice,
    matcher: Matcher,
    id: u64,
    agg: &Mutex<Aggregate>,
) -> String {
    let shared = Arc::new(Mutex::new(Shared { matcher, vdev }));
    let stop = Arc::new(AtomicBool::new(false));

    // Releases held-back keys that never turned into a chord.
    let timer = {
        let (shared, stop) = (shared.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
                let mut g = shared.lock().unwrap();
                let outs = g.matcher.tick(Instant::now());
                if !outs.is_empty() {
                    let _ = g.vdev.emit(&key_events(&outs));
                }
            }
        })
    };

    let reason = loop {
        // Blocks without holding the lock.
        let events = match dev.fetch_events() {
            Ok(e) => e.collect::<Vec<_>>(),
            Err(e) => break e.to_string(),
        };
        let now = Instant::now();
        let mut g = shared.lock().unwrap();
        let mut forward: Vec<InputEvent> = Vec::with_capacity(events.len());
        for ev in events {
            match ev.event_type() {
                EventType::SYNCHRONIZATION => {} // emit() adds its own SYN_REPORT
                EventType::KEY => {
                    let outs = g.matcher.on_key(ev.code(), ev.value(), now);
                    if debug() {
                        eprintln!(
                            "key {:?} value={} -> pass on {} event(s), active={:?}",
                            evdev::KeyCode::new(ev.code()),
                            ev.value(),
                            outs.len(),
                            g.matcher.active()
                        );
                    }
                    forward.extend(key_events(&outs));
                }
                _ => forward.push(ev),
            }
        }
        let active = g.matcher.active();
        agg.lock().unwrap().set(id, active);
        if !forward.is_empty()
            && let Err(e) = g.vdev.emit(&forward)
        {
            break format!("virtual device: {e}");
        }
    };
    stop.store(true, Ordering::Relaxed);
    let _ = timer.join();
    reason
}
