//! Colours a Winry315 keypad according to microphone state (PipeWire/PulseAudio).
//!
//! * yellow – no app is recording from a microphone
//! * green  – apps are using microphone(s) and every one of them is muted
//! * red    – an app is using a microphone that is unmuted

mod keypad;
mod mic;

use keypad::{Color, Keypad};
use mic::MicState;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const REAPPLY_EVERY: Duration = Duration::from_secs(5);
const DEBOUNCE: Duration = Duration::from_millis(100);

fn color_for(state: MicState) -> Color {
    match state {
        MicState::Idle => Color::GOLD,
        MicState::InUseMuted => Color::GREEN,
        MicState::InUseLive => Color::RED,
    }
}

fn main() {
    let (tx, rx) = mpsc::channel();
    mic::spawn_event_watcher(tx);

    let mut keypad = Keypad::new();
    let mut last: Option<MicState> = None;
    let mut last_applied = Instant::now();
    let mut applied = false;
    let mut audio_error: Option<String> = None;

    loop {
        // Audio errors (e.g. `pactl` missing or the sound server not up yet) are
        // logged once per distinct message; keep the last known state meanwhile.
        let state = match mic::current_state() {
            Ok(s) => {
                if audio_error.take().is_some() {
                    eprintln!("audio query recovered");
                }
                Some(s)
            }
            Err(e) => {
                if audio_error.as_ref() != Some(&e) {
                    eprintln!("audio query failed: {e}");
                    audio_error = Some(e);
                }
                last
            }
        };
        if let Some(state) = state {
            update(
                &mut keypad,
                state,
                &mut last,
                &mut applied,
                &mut last_applied,
            );
        }

        // Wait for an audio event (or timeout to re-sync), then debounce bursts.
        if rx.recv_timeout(Duration::from_secs(2)).is_ok() {
            std::thread::sleep(DEBOUNCE);
            while rx.try_recv().is_ok() {}
        }
    }
}

fn update(
    keypad: &mut Keypad,
    state: MicState,
    last: &mut Option<MicState>,
    applied: &mut bool,
    last_applied: &mut Instant,
) {
    {
        let changed = *last != Some(state);
        if changed || !*applied || last_applied.elapsed() >= REAPPLY_EVERY {
            if changed {
                eprintln!("mic state: {state:?}");
            }
            *applied = keypad.set_color(color_for(state));
            if *applied {
                *last_applied = Instant::now();
            }
            *last = Some(state);
        }
    }
}
