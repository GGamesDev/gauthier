//! Flashing the Pico without BOOTSEL (picotool reset interface, then 1200 baud touch).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

use crate::project::Target;
use crate::runner::{console, set_state};
use crate::tools;

const RPI_VID: u16 = 0x2e8a;
/// picorust-runtime and Pico SDK stdio_usb.
const SERIAL_PID: u16 = 0x000a;
const MICROPYTHON_PID: u16 = 0x0005;

/// Runs picotool with a time limit: it can hang while a board is re-enumerating.
fn picotool(args: &[&str]) -> (bool, String) {
    let limit = match args.first().copied() {
        Some("load") | Some("save") => Duration::from_secs(90),
        Some("reboot") => Duration::from_secs(8),
        _ => Duration::from_secs(5),
    };
    let Some(bin) = tools::which("picotool") else {
        return (false, "picotool not found".into());
    };
    let mut child = match tools::command(bin)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (false, e.to_string()),
    };
    // Drain the pipes in the background so picotool never blocks on a full pipe.
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let out_t = thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut so, &mut s);
        s
    });
    let err_t = thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut se, &mut s);
        s
    });
    let end = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if Instant::now() < end => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let mut text = out_t.join().unwrap_or_default();
    text.push_str(&err_t.join().unwrap_or_default());
    match status {
        Some(st) => (st.success(), text),
        None => {
            eprintln!("[gauthier] picotool {} timed out", args.join(" "));
            (false, format!("{text}\npicotool timed out"))
        }
    }
}

fn bootsel_present() -> bool {
    let (ok, out) = picotool(&["info"]);
    ok && !out.contains("No accessible")
}

fn wait_bootsel(timeout: Duration, cancel: &AtomicBool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end && !cancel.load(Ordering::SeqCst) {
        if bootsel_present() {
            return true;
        }
        thread::sleep(Duration::from_millis(400));
    }
    false
}

struct RpiPort {
    path: String,
    pid: u16,
    interface: Option<u8>,
}

/// Serial ports of Raspberry Pi boards.
fn rpi_ports() -> Vec<RpiPort> {
    let mut out: Vec<RpiPort> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(info) if info.vid == RPI_VID => Some(RpiPort {
                // macOS: prefer /dev/cu.* (does not wait for DCD).
                path: p.port_name.replace("/dev/tty.", "/dev/cu."),
                pid: info.pid,
                interface: info.interface,
            }),
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| (a.interface, &a.path).cmp(&(b.interface, &b.path)));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// picorust-runtime exposes two serial ports: the console (lower interface
/// number) and the file service (higher one). Returns (console, files).
fn picorust_ports() -> (Option<String>, Option<String>) {
    let ports: Vec<RpiPort> = rpi_ports().into_iter().filter(|p| p.pid == SERIAL_PID).collect();
    match ports.len() {
        0 => (None, None),
        1 => (Some(ports[0].path.clone()), None),
        _ => (Some(ports[0].path.clone()), Some(ports[ports.len() - 1].path.clone())),
    }
}

pub fn console_port() -> Option<String> {
    picorust_ports().0
}

pub fn files_port() -> Option<String> {
    picorust_ports().1
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct PortInfo {
    pub path: String,
    pub label: String,
    /// A board running picorust-runtime (or Pico SDK stdio).
    pub pico: bool,
}

/// Every serial port, Pico boards first.
pub fn list_ports() -> Vec<PortInfo> {
    let files = files_port();
    let mut out: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !p.port_name.starts_with("/dev/tty.")) // macOS: the cu.* twin is listed too
        .filter(|p| Some(&p.port_name) != files.as_ref()) // the file service is not a console
        .map(|p| {
            let (label, pico) = match &p.port_type {
                serialport::SerialPortType::UsbPort(u) => {
                    let pico = u.vid == RPI_VID && u.pid == SERIAL_PID;
                    let what = match (u.vid, u.pid) {
                        (RPI_VID, SERIAL_PID) => "Raspberry Pi Pico".to_string(),
                        (RPI_VID, MICROPYTHON_PID) => "Pico (MicroPython)".to_string(),
                        _ => u.product.clone().unwrap_or_else(|| format!("USB {:04x}:{:04x}", u.vid, u.pid)),
                    };
                    (what, pico)
                }
                serialport::SerialPortType::BluetoothPort => ("Bluetooth".into(), false),
                _ => ("Serial port".into(), false),
            };
            let short = p.port_name.rsplit('/').next().unwrap_or(&p.port_name).to_string();
            PortInfo { label: format!("{label} ({short})"), path: p.port_name, pico }
        })
        .collect();
    out.sort_by_key(|p| (!p.pico, p.path.clone()));
    out
}

pub fn wait_for_port(timeout: Duration, cancel: &AtomicBool) -> Option<String> {
    let end = Instant::now() + timeout;
    while Instant::now() < end && !cancel.load(Ordering::SeqCst) {
        if let Some(path) = console_port() {
            // Give the OS a moment to finish setting the port up.
            thread::sleep(Duration::from_millis(200));
            return Some(path);
        }
        thread::sleep(Duration::from_millis(200));
    }
    None
}

/// Asks the running firmware over its serial port to reboot into BOOTSEL.
fn request_bootsel_over_serial() -> bool {
    let Some(RpiPort { path, pid, .. }) = rpi_ports().into_iter().next() else { return false };
    if pid == MICROPYTHON_PID {
        // MicroPython: interrupt the program and call machine.bootloader().
        if let Ok(mut p) = serialport::new(&path, 115_200).timeout(Duration::from_millis(200)).open() {
            let _ = p.write_all(b"\r\x03\x03");
            thread::sleep(Duration::from_millis(200));
            let _ = p.write_all(b"import machine; machine.bootloader()\r\n");
            thread::sleep(Duration::from_millis(300));
            return true;
        }
        return false;
    }
    // "1200 baud touch" (picorust-runtime, Pico SDK, Arduino cores).
    match serialport::new(&path, 1200).timeout(Duration::from_millis(200)).open() {
        Ok(mut p) => {
            thread::sleep(Duration::from_millis(100));
            let _ = p.write_data_terminal_ready(false);
            thread::sleep(Duration::from_millis(50));
            true
        }
        Err(_) => false,
    }
}

use std::io::Write;

/// Gets a board into BOOTSEL mode: already there, picotool reset interface,
/// serial request, or (last resort) the user holding BOOTSEL. `wait_user` is the
/// time allowed for the last step.
fn enter_bootsel(app: &AppHandle, cancel: &AtomicBool, wait_user: Duration) -> Result<(), String> {
    let mac_hint = if cfg!(target_os = "macos") {
        " If macOS asks \"Allow accessory to connect?\", click Allow."
    } else {
        ""
    };

    // Keep asking the board to reboot into BOOTSEL for the whole wait: it may be
    // rebooting (just flashed, just read) and only answer a second later.
    let start = Instant::now();
    let deadline = start + Duration::from_secs(8) + wait_user;
    let mut told_user = false;
    let mut attempt = 0u32;
    let mut ready = false;
    while Instant::now() < deadline && !cancel.load(Ordering::SeqCst) {
        if bootsel_present() {
            ready = true;
            break;
        }
        // picorust-runtime / Pico SDK firmware: picotool reset interface…
        let (ok, _) = picotool(&["reboot", "-u", "-f"]);
        // …otherwise the serial port (1200 baud touch, MicroPython).
        let asked = ok || (attempt % 2 == 1 && request_bootsel_over_serial());
        if asked && wait_bootsel(Duration::from_secs(4), cancel) {
            ready = true;
            break;
        }
        attempt += 1;
        if !told_user && start.elapsed() > Duration::from_secs(8) {
            told_user = true;
            console(
                app,
                "info",
                format!("Waiting for the Pico: hold its BOOTSEL button while plugging it in (only needed once).{mac_hint}\n"),
            );
            set_state(app, "flashing", "Waiting for a Pico in BOOTSEL mode…");
        }
        thread::sleep(Duration::from_millis(1500));
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("Stopped.".into());
    }
    if !ready {
        return Err("No Pico found.".into());
    }
    Ok(())
}

fn last_line(out: &str) -> String {
    out.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string()
}

/// Flashes the program and starts it.
/// Chip of the board in BOOTSEL mode, read from its boot ROM.
fn bootsel_chip() -> Option<Target> {
    let (ok, out) = picotool(&["info", "-d"]);
    if !ok {
        return None;
    }
    let chip = out.lines().find_map(|l| l.trim().strip_prefix("type:").map(|t| t.trim().to_string()))?;
    if chip.starts_with("RP2040") {
        Some(Target::Pico)
    } else if chip.starts_with("RP235") {
        Some(Target::Pico2)
    } else {
        None
    }
}

fn board_name(t: Target) -> &'static str {
    match t {
        Target::Pico => "Raspberry Pi Pico (RP2040)",
        Target::Pico2 => "Raspberry Pi Pico 2 (RP2350)",
        Target::Desktop => "this computer",
    }
}

#[derive(serde::Serialize, Clone)]
pub struct BoardMismatch {
    pub board: Target,
    pub target: Target,
}

pub fn flash(app: &AppHandle, elf: &Path, target: Target, cancel: &AtomicBool) -> Result<(), String> {
    enter_bootsel(app, cancel, Duration::from_secs(120))?;

    // A program built for the other chip would not start (the board would stay in BOOTSEL).
    if let Some(board) = bootsel_chip() {
        if board != target {
            // Give the board back its current program (if it has a valid one).
            let _ = picotool(&["reboot"]);
            let _ = app.emit("board-mismatch", BoardMismatch { board, target });
            return Err(format!(
                "This board is a {}, but the program was built for the {}. Choose \"{}\" in the target menu and press Run again.",
                board_name(board),
                board_name(target),
                board_name(board)
            ));
        }
    }

    set_state(app, "flashing", "Writing the program to the Pico…");
    let elf = elf.to_string_lossy();
    let (ok, out) = picotool(&["load", "-u", "-v", "-x", &elf, "-t", "elf"]);
    if !ok {
        return Err(format!("Flashing failed: {}", last_line(&out)));
    }
    console(app, "info", "Program sent to the Pico.\n");
    Ok(())
}

/// Waits for the file service of a freshly started program.
pub fn wait_for_files_port(timeout: Duration) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if files_port().is_some() {
            thread::sleep(Duration::from_millis(200));
            return true;
        }
        thread::sleep(Duration::from_millis(150));
    }
    false
}
