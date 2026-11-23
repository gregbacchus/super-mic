//! Winry315 lighting over the VIA raw-HID interface (protocol <= 0x0A, "legacy"
//! lighting ids: 0x80 brightness, 0x81 effect, 0x83 colour as hue+saturation).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::thread::sleep;
use std::time::{Duration, Instant};

const HID_ID: &str = "HID_ID=0003:0000F1F1:00000315";
/// Usage page 0xFF60 (QMK raw HID), as it appears in the report descriptor.
const RAW_HID_USAGE_PAGE: [u8; 3] = [0x06, 0x60, 0xff];
const REPORT_LEN: usize = 32;
const O_NONBLOCK: i32 = 0o4000;

const CMD_SET_VALUE: u8 = 0x07;
const ID_BRIGHTNESS: u8 = 0x80;
const ID_EFFECT: u8 = 0x81;
const ID_COLOR: u8 = 0x83;
const EFFECT_SOLID: u8 = 1;
/// Full brightness (the firmware caps it at 150 anyway).
const BRIGHTNESS: u8 = 150;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    /// 0–255 (maps onto 0–360°)
    pub hue: u8,
    pub sat: u8,
    /// Brightness 0–255 (the firmware caps it at 150).
    pub val: u8,
}

impl Color {
    pub const RED: Color = Color {
        hue: 0,
        sat: 255,
        val: BRIGHTNESS,
    };
    /// Golden yellow (~42°), dimmer than the "in use" colours.
    pub const GOLD: Color = Color {
        hue: 30,
        sat: 255,
        val: 60,
    };
    pub const GREEN: Color = Color {
        hue: 85,
        sat: 255,
        val: BRIGHTNESS,
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Unknown,
    Connected,
    /// Not plugged in (an expected situation, not an error).
    Absent,
    /// Found but unusable (e.g. permissions); logged once per distinct message.
    Failed(String),
}

enum OpenError {
    NotFound,
    Other(String),
}

pub struct Keypad {
    dev: Option<File>,
    status: Status,
}

impl Keypad {
    pub fn new() -> Self {
        Keypad {
            dev: None,
            status: Status::Unknown,
        }
    }

    fn transition(&mut self, new: Status) {
        if self.status == new {
            return;
        }
        match (&self.status, &new) {
            (Status::Connected, Status::Absent) => eprintln!("keypad disconnected"),
            (_, Status::Absent) => eprintln!("keypad not connected, waiting for it"),
            (_, Status::Connected) => eprintln!("keypad connected"),
            (_, Status::Failed(e)) => eprintln!("keypad unusable: {e}"),
            (_, Status::Unknown) => {}
        }
        self.status = new;
    }

    /// Returns true if the colour was applied. A missing keypad is not an
    /// error: it is logged once on connect/disconnect and retried by the caller.
    pub fn set_color(&mut self, c: Color) -> bool {
        if self.dev.is_none() {
            match open() {
                Ok(f) => self.dev = Some(f),
                Err(OpenError::NotFound) => {
                    self.transition(Status::Absent);
                    return false;
                }
                Err(OpenError::Other(e)) => {
                    self.transition(Status::Failed(e));
                    return false;
                }
            }
        }
        match send_color(self.dev.as_mut().unwrap(), c) {
            Ok(()) => {
                self.transition(Status::Connected);
                true
            }
            Err(e) => {
                // Unplugged (or went away mid-write): rediscover next time.
                self.dev = None;
                if self.status == Status::Connected {
                    self.transition(Status::Absent);
                } else {
                    self.transition(Status::Failed(e));
                }
                false
            }
        }
    }
}

fn send_color(dev: &mut File, c: Color) -> Result<(), String> {
    transact(dev, &[CMD_SET_VALUE, ID_EFFECT, EFFECT_SOLID])?;
    transact(dev, &[CMD_SET_VALUE, ID_COLOR, c.hue, c.sat])?;
    transact(dev, &[CMD_SET_VALUE, ID_BRIGHTNESS, c.val])
}

fn open() -> Result<File, OpenError> {
    let entries = fs::read_dir("/sys/class/hidraw").map_err(|e| OpenError::Other(e.to_string()))?;
    for entry in entries.flatten() {
        let dir = entry.path().join("device");
        let is_pad = fs::read_to_string(dir.join("uevent")).is_ok_and(|u| u.contains(HID_ID));
        let is_raw = fs::read(dir.join("report_descriptor"))
            .is_ok_and(|d| d.windows(3).any(|w| w == RAW_HID_USAGE_PAGE));
        if is_pad && is_raw {
            let path = format!("/dev/{}", entry.file_name().to_string_lossy());
            return OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(O_NONBLOCK)
                .open(&path)
                .map_err(|e| OpenError::Other(format!("opening {path}: {e}")));
        }
    }
    Err(OpenError::NotFound)
}

/// Writes one report and waits for the keyboard's reply (which must be drained).
fn transact(dev: &mut File, msg: &[u8]) -> Result<(), String> {
    let mut out = [0u8; REPORT_LEN + 1]; // out[0] = report id 0
    out[1..=msg.len()].copy_from_slice(msg);
    dev.write_all(&out).map_err(|e| format!("write: {e}"))?;

    let mut reply = [0u8; REPORT_LEN];
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match dev.read(&mut reply) {
            Ok(n) if n > 0 => return Ok(()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(format!("read: {e}")),
        }
        if Instant::now() > deadline {
            return Err("timed out waiting for keypad reply".into());
        }
        sleep(Duration::from_millis(2));
    }
}
