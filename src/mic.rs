//! Microphone state, read through `pactl` (works with pipewire-pulse on Wayland).

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
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

pub fn current_state() -> Result<MicState, String> {
    let sources = pactl_json("sources")?;
    let outputs = pactl_json("source-outputs")?;

    let mut in_use = false;
    let mut live = false;
    for o in &outputs {
        if o["corked"].as_bool().unwrap_or(false) || is_level_meter(o) {
            continue;
        }
        let Some(src) = sources.iter().find(|s| s["index"] == o["source"]) else {
            continue;
        };
        if is_monitor(src) {
            continue; // capturing system audio, not a microphone
        }
        in_use = true;
        let muted = src["mute"].as_bool().unwrap_or(false) || o["mute"].as_bool().unwrap_or(false);
        live |= !muted;
    }
    Ok(match (in_use, live) {
        (false, _) => MicState::Idle,
        (true, false) => MicState::InUseMuted,
        (true, true) => MicState::InUseLive,
    })
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
