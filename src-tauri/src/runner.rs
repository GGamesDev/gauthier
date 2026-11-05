//! Build, run, flash, and the console session (program stdin/stdout or Pico serial port).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::pico;
use crate::project::{self, Target};
use crate::pico_fs::PicoFs;
use crate::tools;

#[derive(Serialize, Clone)]
pub struct ConsoleEvent<'a> {
    /// "out", "err", "info", "build", "error"
    pub kind: &'a str,
    pub text: String,
}

#[derive(Serialize, Clone)]
pub struct RunState {
    /// "building", "flashing", "running", "idle"
    pub state: &'static str,
    pub message: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct Diagnostic {
    pub level: String,
    pub message: String,
    /// 1-based; 0 when the error is not in the user's file.
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub label: Option<String>,
    pub rendered: String,
    /// rustc error code (E0308) or lint name (unused_variables).
    pub code: Option<String>,
}

pub fn console(app: &AppHandle, kind: &str, text: impl Into<String>) {
    let _ = app.emit("console", ConsoleEvent { kind, text: text.into() });
}

pub fn set_state(app: &AppHandle, state: &'static str, message: impl Into<String>) {
    let _ = app.emit("run-state", RunState { state, message: message.into() });
}

enum Session {
    Process { child: Arc<Mutex<Child>>, stdin: Option<ChildStdin> },
    Serial { writer: Box<dyn serialport::SerialPort>, stop: Arc<AtomicBool>, reader: Option<thread::JoinHandle<()>> },
}

#[derive(Default)]
pub struct Runner {
    session: Mutex<Option<Session>>,
    busy: AtomicBool,
    cancel: Arc<AtomicBool>,
    /// Incremented when a session is replaced: threads of older sessions stay silent.
    generation: Arc<AtomicU64>,
    /// Closing a serial port can take a while on macOS: done in the background.
    closing: Mutex<Option<thread::JoinHandle<()>>>,
    pub files: Arc<PicoFs>,
}

impl Runner {
    /// Ends the current session. The port is really closed when this returns.
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        let session = self.session.lock().unwrap().take();
        if let Some(s) = session {
            match s {
                Session::Process { child, .. } => {
                    let _ = child.lock().unwrap().kill();
                }
                Session::Serial { stop, reader, writer } => {
                    stop.store(true, Ordering::SeqCst);
                    let closer = thread::spawn(move || {
                        let t0 = std::time::Instant::now();
                        // Pending output would make close() wait for it to drain.
                        let _ = writer.clear(serialport::ClearBuffer::All);
                        drop(writer);
                        if let Some(r) = reader {
                            let _ = r.join();
                        }
                        eprintln!("[gauthier] serial port closed in {} ms", t0.elapsed().as_millis());
                    });
                    *self.closing.lock().unwrap() = Some(closer);
                }
            }
        }
    }

    /// Waits until the previous serial port is really closed (it is opened exclusively).
    fn wait_closed(&self) {
        let handle = self.closing.lock().unwrap().take();
        if let Some(h) = handle {
            let _ = h.join();
        }
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    pub fn send_input(&self, text: &str) -> Result<(), String> {
        let mut guard = self.session.lock().unwrap();
        match guard.as_mut() {
            Some(Session::Process { stdin: Some(stdin), .. }) => {
                stdin.write_all(text.as_bytes()).and_then(|_| stdin.flush()).map_err(|e| e.to_string())
            }
            Some(Session::Serial { writer, .. }) => {
                writer.write_all(text.as_bytes()).and_then(|_| writer.flush()).map_err(|e| e.to_string())
            }
            _ => Err("No program is running".into()),
        }
    }

    /// Serial monitor: connects to a port without flashing anything.
    pub fn connect(self: &Arc<Self>, app: AppHandle, path: Option<String>, baud: u32) -> Result<(), String> {
        if self.busy.load(Ordering::SeqCst) {
            return Err("Busy, try again in a moment.".into());
        }
        self.stop();
        self.cancel.store(false, Ordering::SeqCst);
        let path = match path {
            Some(p) => p,
            None => pico::wait_for_port(Duration::from_millis(500), &self.cancel).ok_or("No Pico found on USB.")?,
        };
        self.start_serial(&app, Some(path), baud)
    }

    /// Builds a debug binary for the debugger. Returns (executable, source file) on success.
    pub fn build_for_debug(&self, app: &AppHandle, code: &str, runtime_dir: &Path) -> Result<Option<(std::path::PathBuf, std::path::PathBuf)>, String> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err("Busy, try again in a moment.".into());
        }
        self.stop();
        self.cancel.store(false, Ordering::SeqCst);
        let result = (|| {
            if !tools::status().desktop_ready() {
                return Err("Some tools are missing: click \"Install tools\" first.".to_string());
            }
            set_state(app, "building", "Building for the debugger…");
            let dir = project::prepare(Target::Desktop, code, runtime_dir, project::Purpose::Build)?;
            if !cargo_build(app, &dir, false)? {
                set_state(app, "idle", "Build failed");
                return Ok(None);
            }
            Ok(Some((project::elf_path(Target::Desktop), dir.join(project::MAIN_RS))))
        })();
        self.busy.store(false, Ordering::SeqCst);
        result
    }

    /// Full "Run": build, then run on the computer or flash the Pico and open its console.
    pub fn run(self: &Arc<Self>, app: AppHandle, code: String, name: String, target: Target, args: Vec<String>, runtime_dir: std::path::PathBuf) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.stop();
        self.cancel.store(false, Ordering::SeqCst);
        let this = self.clone();
        thread::spawn(move || {
            let result = this.run_inner(&app, &code, &name, target, &args, &runtime_dir);
            this.busy.store(false, Ordering::SeqCst);
            if let Err(e) = result {
                console(&app, "error", format!("{e}\n"));
                set_state(&app, "idle", "");
            }
        });
    }

    fn run_inner(&self, app: &AppHandle, code: &str, _name: &str, target: Target, args: &[String], runtime_dir: &Path) -> Result<(), String> {
        let st = tools::status();
        let ready = if target == Target::Desktop { st.desktop_ready() } else { st.pico_ready() };
        if !ready {
            return Err("Some tools are missing: click \"Install tools\" first.".into());
        }

        set_state(app, "building", "Building…");
        let dir = project::prepare(target, code, runtime_dir, project::Purpose::Build)?;
        let ok = cargo_build(app, &dir, target != Target::Desktop)?;
        if !ok {
            set_state(app, "idle", "Build failed");
            return Ok(());
        }
        if self.cancel.load(Ordering::SeqCst) {
            set_state(app, "idle", "Stopped");
            return Ok(());
        }

        match target {
            Target::Desktop => self.start_process(app, &project::elf_path(target), args),
            _ => {
                set_state(app, "flashing", "Sending to the Pico…");
                self.files.reset();
                pico::flash(app, &project::elf_path(target), &self.cancel)?;
                // Thonny-like: the source of the program on the board is main.rs.
                if pico::wait_for_files_port(Duration::from_secs(6)) {
                    match self.files.write("main.rs", code.as_bytes()) {
                        Ok(()) => console(app, "info", "Saved as main.rs on the Pico.\n"),
                        Err(e) => console(app, "err", format!("Could not save main.rs on the Pico: {e}\n")),
                    }
                    let _ = app.emit("pico-files-changed", ());
                } else {
                    console(app, "err", "The Pico's file service did not start (old picorust-runtime?).\n");
                }
                self.start_serial(app, None, 115_200)
            }
        }
    }

    fn start_process(&self, app: &AppHandle, exe: &Path, args: &[String]) -> Result<(), String> {
        let mut child = tools::command(exe)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Cannot start the program: {e}"))?;
        let stdin = child.stdin.take();
        forward(app.clone(), child.stdout.take().unwrap(), "out");
        forward(app.clone(), child.stderr.take().unwrap(), "err");
        let child = Arc::new(Mutex::new(child));
        *self.session.lock().unwrap() = Some(Session::Process { child: child.clone(), stdin });
        set_state(app, "running", "Running");

        let app = app.clone();
        let generation = self.generation.clone();
        let mine = generation.load(Ordering::SeqCst);
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(50));
            let status = child.lock().unwrap().try_wait();
            match status {
                Ok(Some(st)) => {
                    // Let the output threads flush first.
                    thread::sleep(Duration::from_millis(100));
                    let msg = match st.code() {
                        Some(0) => "Program finished.".to_string(),
                        Some(c) => format!("Program exited with code {c}."),
                        None => "Program stopped.".to_string(),
                    };
                    console(&app, "info", format!("{msg}\n"));
                    if generation.load(Ordering::SeqCst) == mine {
                        set_state(&app, "idle", msg);
                    }
                    break;
                }
                Ok(None) => {}
                Err(_) => break,
            }
        });
        Ok(())
    }

    fn start_serial(&self, app: &AppHandle, path: Option<String>, baud: u32) -> Result<(), String> {
        let path = match path {
            Some(p) => p,
            None => pico::wait_for_port(Duration::from_secs(8), &self.cancel)
                .ok_or("The Pico did not show up as a serial port. Does your program call picorust_runtime::init()?")?,
        };
        self.wait_closed();
        let mut port = serialport::new(&path, baud)
            .timeout(Duration::from_millis(100))
            .open()
            .map_err(|e| format!("Cannot open {path}: {e}"))?;
        let _ = port.write_data_terminal_ready(true);
        let writer = port.try_clone().map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        console(app, "info", format!("Connected to {path} at {baud} baud.\n"));
        set_state(app, "running", "Serial monitor connected");

        let app = app.clone();
        let generation = self.generation.clone();
        let mine = generation.load(Ordering::SeqCst);
        let stop2 = stop.clone();
        let reader = thread::spawn(move || {
            let stop = stop2;
            let mut buf = [0u8; 512];
            let mut pending: Vec<u8> = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                match port.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        pending.extend_from_slice(&buf[..n]);
                        let text = take_utf8(&mut pending);
                        if !text.is_empty() {
                            console(&app, "out", text);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(_) => {
                        if generation.load(Ordering::SeqCst) == mine {
                            console(&app, "info", "The board was disconnected.\n");
                            set_state(&app, "idle", "Disconnected");
                        }
                        return;
                    }
                }
            }
            // Stopped on purpose: whoever stopped us reports the new state.
        });
        *self.session.lock().unwrap() = Some(Session::Serial { writer, stop, reader: Some(reader) });
        Ok(())
    }
}

/// Takes the longest valid UTF-8 prefix (keeps an incomplete trailing character for later).
fn take_utf8(pending: &mut Vec<u8>) -> String {
    let valid = match std::str::from_utf8(pending) {
        Ok(_) => pending.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => pending.len(), // invalid bytes: let lossy conversion replace them
    };
    let text = String::from_utf8_lossy(&pending[..valid]).to_string();
    pending.drain(..valid);
    text
}

fn forward(app: AppHandle, mut stream: impl Read + Send + 'static, kind: &'static str) {
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut pending = Vec::new();
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    let text = take_utf8(&mut pending);
                    if !text.is_empty() {
                        console(&app, kind, text);
                    }
                }
            }
        }
    });
}

/// One compiler message of `cargo … --message-format=json`, if it is an error or a
/// warning about the user's file.
pub(crate) fn parse_diagnostic(v: &serde_json::Value) -> Option<Diagnostic> {
    if v["reason"] != "compiler-message" {
        return None;
    }
    let msg = &v["message"];
    let level = msg["level"].as_str().unwrap_or("").to_string();
    if !matches!(level.as_str(), "error" | "warning") {
        return None;
    }
    let message = msg["message"].as_str().unwrap_or("").to_string();
    if message.starts_with("aborting due to") || message.ends_with("warnings emitted") || message.ends_with("warning emitted") {
        return None;
    }
    // Only diagnostics about the user's file (not dependencies).
    let target_src = v["target"]["src_path"].as_str().unwrap_or("");
    if !target_src.ends_with(project::MAIN_RS.replace('/', std::path::MAIN_SEPARATOR_STR).as_str())
        && !target_src.ends_with(project::MAIN_RS)
    {
        return None;
    }
    let rendered = msg["rendered"].as_str().unwrap_or("").to_string();
    let span = msg["spans"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["is_primary"] == true && s["file_name"] == project::MAIN_RS));
    let n = |s: Option<&serde_json::Value>, k: &str| s.and_then(|s| s[k].as_u64()).unwrap_or(0) as u32;
    Some(Diagnostic {
        level,
        message,
        line: n(span, "line_start"),
        column: n(span, "column_start"),
        end_line: n(span, "line_end"),
        end_column: n(span, "column_end"),
        label: span.and_then(|s| s["label"].as_str()).map(str::to_string),
        rendered,
        code: msg["code"]["code"].as_str().map(str::to_string),
    })
}

/// `cargo build` with JSON diagnostics. Emits "diagnostics" and console lines. Returns success.
fn cargo_build(app: &AppHandle, dir: &Path, release: bool) -> Result<bool, String> {
    let cargo = tools::which("cargo").ok_or("cargo not found")?;
    let mut cmd = tools::command(cargo);
    cmd.arg("build").arg("--message-format=json-diagnostic-rendered-ansi").current_dir(dir);
    if release {
        cmd.arg("--release");
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;

    // stderr: Cargo's progress ("Compiling …").
    let stderr = child.stderr.take().unwrap();
    let app2 = app.clone();
    let progress = thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let t = line.trim_start();
            if t.starts_with("Compiling") || t.starts_with("Downloaded") || t.starts_with("Updating") {
                console(&app2, "build", format!("{t}\n"));
            } else if t.starts_with("error") {
                console(&app2, "error", format!("{t}\n"));
            }
        }
    });

    let mut diags = Vec::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let Some(d) = parse_diagnostic(&v) else { continue };
        let rendered = d.rendered.clone();
        if d.level == "error" {
            console(app, "error", rendered);
        }
        diags.push(d);
    }
    let _ = progress.join();
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = app.emit("diagnostics", &diags);
    Ok(status.success())
}
