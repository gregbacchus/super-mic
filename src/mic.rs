//! Microphone state, read through `pactl` (works with pipewire-pulse on Wayland).

use crate::hotkey::Hold;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicState {
    /// No app is recording from a microphone.
    Idle,
    /// Apps are recording, and all the microphones involved are muted.
    InUseMuted,
    /// At least one microphone in use by an app is unmuted.
    InUseLive,
}

fn pactl_json(what: &str) -> Result<Vec<Value>, String> {
    let out = Command::new("pactl")
        .args(["-f", "json", "list", what])
        .output()
        .map_err(|e| format!("running pactl: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    match serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())? {
        Value::Array(a) => Ok(a),
        _ => Err("unexpected pactl output".into()),
    }
}

fn is_monitor(source: &Value) -> bool {
    source["name"]
        .as_str()
        .is_some_and(|n| n.ends_with(".monitor"))
        || source["properties"]["device.class"] == "monitor"
}

/// Streams that only measure levels (volume meters) don't count as "using" the mic.
fn is_level_meter(output: &Value) -> bool {
    let p = &output["properties"];
    p["application.id"] == "org.PulseAudio.pavucontrol"
        || p["media.name"].as_str().is_some_and(|n| n == "Peak detect")
}

/// (source, recording stream) pairs where an app is using a microphone.
fn mics_in_use<'a>(sources: &'a [Value], outputs: &'a [Value]) -> Vec<(&'a Value, &'a Value)> {
    outputs
        .iter()
        .filter(|o| !o["corked"].as_bool().unwrap_or(false) && !is_level_meter(o))
        .filter_map(|o| {
            let src = sources.iter().find(|s| s["index"] == o["source"])?;
            // A monitor captures system audio, not a microphone.
            (!is_monitor(src)).then_some((src, o))
        })
        .collect()
}

pub fn current_state() -> Result<MicState, String> {
    let sources = pactl_json("sources")?;
    let outputs = pactl_json("source-outputs")?;

    let in_use = mics_in_use(&sources, &outputs);
    let live = in_use.iter().any(|(src, o)| {
        !(src["mute"].as_bool().unwrap_or(false) || o["mute"].as_bool().unwrap_or(false))
    });
    Ok(match (in_use.is_empty(), live) {
        (true, _) => MicState::Idle,
        (false, false) => MicState::InUseMuted,
        (false, true) => MicState::InUseLive,
    })
}

/// Names of microphones an app is currently using.
fn in_use_source_names() -> Result<Vec<String>, String> {
    let sources = pactl_json("sources")?;
    let outputs = pactl_json("source-outputs")?;
    let mut names: Vec<String> = mics_in_use(&sources, &outputs)
        .iter()
        .filter_map(|(src, _)| src["name"].as_str().map(String::from))
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// Should "toggle" mute (true) or unmute (false)? It inverts what the user
/// cares about: the mic(s) apps are using. If any of those was live, mute;
/// if they were all muted, unmute. With nothing in use, fall back to all mics.
/// `original` holds each mic's mute state from before the override.
fn toggle_target(original: &HashMap<String, bool>, in_use: &[String]) -> bool {
    let live = |name: &String| original.get(name).is_some_and(|muted| !muted);
    if in_use.is_empty() {
        original.keys().any(live)
    } else {
        in_use.iter().any(live)
    }
}

/// Sends a unit message whenever a source or source-output changes.
/// Respawns `pactl subscribe` if it dies.
pub fn spawn_event_watcher(tx: Sender<()>) {
    std::thread::spawn(move || {
        let mut last_error = String::new();
        loop {
            let child = Command::new("pactl")
                .arg("subscribe")
                .stdout(Stdio::piped())
                .spawn();
            match child {
                Ok(mut child) => {
                    let stdout = child.stdout.take().expect("piped stdout");
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        if (line.contains(" on source ") || line.contains(" on source-output "))
                            && tx.send(()).is_err()
                        {
                            return;
                        }
                    }
                    let _ = child.wait();
                }
                Err(e) => {
                    // Log once per distinct failure, not on every retry.
                    if last_error != e.to_string() {
                        eprintln!("pactl subscribe: {e}");
                        last_error = e.to_string();
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

/// Non-monitor sources (microphones) with their mute state.
fn list_mics() -> Result<Vec<(String, bool)>, String> {
    Ok(pactl_json("sources")?
        .iter()
        .filter(|s| !is_monitor(s))
        .filter_map(|s| Some((s["name"].as_str()?.to_string(), s["mute"].as_bool()?)))
        .collect())
}

fn set_mute(source: &str, mute: bool) {
    let status = Command::new("pactl")
        .args(["set-source-mute", source, if mute { "1" } else { "0" }])
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        eprintln!("failed to set mute={mute} on {source}");
    }
}

/// How long an unmuting request waits for a longer combination to complete
/// before the mics are actually unmuted. Muting is never delayed, so a sloppy
/// chord can only ever err towards muted.
const UNMUTE_SETTLE: Duration = Duration::from_millis(60);

/// Applies hold overrides to **all** microphones; each mic's previous state is
/// restored when the override ends (`None`).
pub fn spawn_mute_controller(rx: Receiver<Option<Hold>>) {
    std::thread::spawn(move || {
        let mut saved: HashMap<String, bool> = HashMap::new();
        let mut next: Option<Option<Hold>> = None;
        loop {
            let want = match next.take() {
                Some(w) => w,
                None => match rx.recv() {
                    Ok(w) => w,
                    Err(_) => return,
                },
            };
            if matches!(want, Some(Hold::Unmute | Hold::Toggle)) {
                match rx.recv_timeout(UNMUTE_SETTLE) {
                    Ok(newer) => {
                        next = Some(newer);
                        continue;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            apply(want, &mut saved);
        }
    });
}

fn apply(want: Option<Hold>, saved: &mut HashMap<String, bool>) {
    let Some(hold) = want else {
        for (name, was_muted) in saved.drain() {
            set_mute(&name, was_muted);
        }
        eprintln!("hotkey released, mute state restored");
        return;
    };
    let mics = match list_mics() {
        Ok(m) => m,
        Err(e) => return eprintln!("hotkey: listing microphones failed: {e}"),
    };
    // Remember each mic's state from before the first override.
    for (name, was_muted) in &mics {
        saved.entry(name.clone()).or_insert(*was_muted);
    }
    let mute = match hold {
        Hold::Mute => true,
        Hold::Unmute => false,
        Hold::Toggle => toggle_target(saved, &in_use_source_names().unwrap_or_default()),
    };
    for (name, _) in &mics {
        set_mute(name, mute);
    }
    eprintln!(
        "hotkey: {hold:?} while held (microphones {})",
        if mute { "muted" } else { "unmuted" }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(pairs: &[(&str, bool)]) -> HashMap<String, bool> {
        pairs.iter().map(|(n, m)| (n.to_string(), *m)).collect()
    }

    #[test]
    fn toggle_unmutes_when_the_mic_in_use_is_muted_even_if_others_are_live() {
        let original = state(&[("usb", true), ("builtin", false)]);
        assert!(!toggle_target(&original, &["usb".into()]));
    }

    #[test]
    fn toggle_mutes_when_the_mic_in_use_is_live() {
        let original = state(&[("usb", false), ("builtin", true)]);
        assert!(toggle_target(&original, &["usb".into()]));
    }

    #[test]
    fn toggle_with_nothing_in_use_looks_at_all_mics() {
        assert!(toggle_target(&state(&[("a", true), ("b", false)]), &[]));
        assert!(!toggle_target(&state(&[("a", true), ("b", true)]), &[]));
    }

    #[test]
    fn in_use_ignores_corked_monitors_and_meters() {
        let sources: Vec<Value> = serde_json::from_str(
            r#"[{"index":1,"name":"mic","mute":false},{"index":2,"name":"out.monitor","mute":false}]"#,
        )
        .unwrap();
        let outputs: Vec<Value> = serde_json::from_str(
            r#"[{"source":1,"corked":true},
                {"source":2,"corked":false},
                {"source":1,"corked":false,"properties":{"application.id":"org.PulseAudio.pavucontrol"}},
                {"source":1,"corked":false,"properties":{"application.name":"Zoom"}}]"#,
        )
        .unwrap();
        let used = mics_in_use(&sources, &outputs);
        assert_eq!(used.len(), 1);
        assert_eq!(used[0].1["properties"]["application.name"], "Zoom");
    }
}
