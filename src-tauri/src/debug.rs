//! Step-by-step debugging of programs on the computer, through lldb-dap
//! (Debug Adapter Protocol over stdio, same framing as LSP).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::runner::{console, set_state};
use crate::tools;

type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>;

#[derive(Serialize, Clone, Debug)]
pub struct Frame {
    pub id: i64,
    pub name: String,
    /// 1-based line in the user's file (0 when the frame is elsewhere, e.g. in std).
    pub line: u32,
    pub in_user_code: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    /// > 0: the variable has children (fetch them with `debug_variables`).
    pub reference: i64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Stopped {
    pub reason: String,
    pub frames: Vec<Frame>,
    pub variables: Vec<Variable>,
}

struct Session {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    seq: Arc<Mutex<u64>>,
    thread_id: Arc<Mutex<i64>>,
    source: PathBuf,
}

#[derive(Default)]
pub struct Debugger {
    session: Mutex<Option<Session>>,
}

fn send(stdin: &Mutex<ChildStdin>, msg: &Value) -> Result<(), String> {
    let body = msg.to_string();
    let mut w = stdin.lock().unwrap();
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body).map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())
}

fn request_raw(stdin: &Mutex<ChildStdin>, pending: &Pending, seq: &Mutex<u64>, command: &str, args: Value, timeout: Duration) -> Result<Value, String> {
    let id = {
        let mut s = seq.lock().unwrap();
        *s += 1;
        *s
    };
    let (tx, rx) = mpsc::channel();
    pending.lock().unwrap().insert(id, tx);
    send(stdin, &json!({"seq": id, "type": "request", "command": command, "arguments": args}))?;
    let resp = rx.recv_timeout(timeout).map_err(|_| format!("the debugger did not answer ({command})"))?;
    if resp["success"] == false {
        return Err(resp["message"].as_str().unwrap_or("debugger error").to_string());
    }
    Ok(resp["body"].clone())
}

/// lldb-dap: Xcode / Command Line Tools on macOS, the lldb package on Linux.
fn find_lldb_dap() -> Option<PathBuf> {
    if let Some(p) = tools::which("lldb-dap").or_else(|| tools::which("lldb-vscode")) {
        return Some(p);
    }
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("xcrun").args(["--find", "lldb-dap"]).output().ok()?;
        let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn rust_lldb_init() -> Vec<String> {
    let Some(rustc) = tools::which("rustc") else { return vec![] };
    let Ok(out) = tools::command(rustc).args(["--print", "sysroot"]).output() else { return vec![] };
    let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let lookup = Path::new(&sysroot).join("lib/rustlib/etc/lldb_lookup.py");
    if lookup.is_file() {
        // Pretty display of String, Vec, Option… (what rust-lldb does).
        vec![format!("command script import \"{}\"", lookup.display())]
    } else {
        vec![]
    }
}

impl Session {
    fn request(&self, command: &str, args: Value) -> Result<Value, String> {
        request_raw(&self.stdin, &self.pending, &self.seq, command, args, Duration::from_secs(15))
    }
}

/// Collects the state shown when the program stops: call stack and local variables.
fn snapshot(stdin: &Mutex<ChildStdin>, pending: &Pending, seq: &Mutex<u64>, thread_id: i64, source: &Path, reason: &str) -> Result<Stopped, String> {
    let t = Duration::from_secs(10);
    let st = request_raw(stdin, pending, seq, "stackTrace", json!({"threadId": thread_id, "startFrame": 0, "levels": 30}), t)?;
    let source_name = source.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let frames: Vec<Frame> = st["stackFrames"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| {
            let path = f["source"]["path"].as_str().unwrap_or("");
            let mine = Path::new(path) == source || (!path.is_empty() && path.ends_with(&format!("src/{source_name}")));
            Frame {
                id: f["id"].as_i64().unwrap_or(0),
                // "program::main" → "main"
                name: f["name"].as_str().unwrap_or("?").trim_start_matches("program::").to_string(),
                line: if mine { f["line"].as_u64().unwrap_or(0) as u32 } else { 0 },
                in_user_code: mine,
            }
        })
        .collect();
    // Variables of the first frame that is in the user's file.
    let mut variables = Vec::new();
    if let Some(frame) = frames.iter().find(|f| f.in_user_code) {
        let scopes = request_raw(stdin, pending, seq, "scopes", json!({"frameId": frame.id}), t)?;
        if let Some(locals) = scopes["scopes"].as_array().and_then(|s| s.iter().find(|s| s["name"].as_str().map(|n| n.starts_with("Local")).unwrap_or(false)).or(s.first())) {
            let r = locals["variablesReference"].as_i64().unwrap_or(0);
            variables = fetch_variables(stdin, pending, seq, r)?;
        }
    }
    Ok(Stopped { reason: reason.to_string(), frames, variables })
}

fn fetch_variables(stdin: &Mutex<ChildStdin>, pending: &Pending, seq: &Mutex<u64>, reference: i64) -> Result<Vec<Variable>, String> {
    if reference <= 0 {
        return Ok(vec![]);
    }
    let v = request_raw(stdin, pending, seq, "variables", json!({"variablesReference": reference}), Duration::from_secs(10))?;
    Ok(v["variables"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|x| Variable {
            name: x["name"].as_str().unwrap_or("").to_string(),
            value: x["value"].as_str().unwrap_or("").to_string(),
            type_name: x["type"].as_str().map(str::to_string),
            reference: x["variablesReference"].as_i64().unwrap_or(0),
        })
        .collect())
}

impl Debugger {
    pub fn start(&self, app: AppHandle, exe: &Path, args: &[String], source: &Path, breakpoints: &[u32]) -> Result<(), String> {
        self.stop();
        let dap = find_lldb_dap().ok_or(
            "The debugger (lldb-dap) was not found. On macOS it comes with Apple's Command Line Tools; on Linux install the lldb package.",
        )?;
        let mut child = tools::command(dap)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Cannot start the debugger: {e}"))?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let stdout = child.stdout.take().unwrap();
        let pending: Pending = Arc::default();
        let seq = Arc::new(Mutex::new(0u64));
        let thread_id = Arc::new(Mutex::new(0i64));
        let (init_tx, init_rx) = mpsc::channel::<()>();

        // Reader: answers to requests, events (stopped, output, exited…).
        {
            let (stdin, pending, seq, thread_id) = (stdin.clone(), pending.clone(), seq.clone(), thread_id.clone());
            let source = source.to_path_buf();
            let app = app.clone();
            thread::spawn(move || {
                let mut r = BufReader::new(stdout);
                loop {
                    let mut len = 0usize;
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 {
                            let _ = app.emit("debug-ended", ());
                            return;
                        }
                        let line = line.trim_end();
                        if line.is_empty() {
                            break;
                        }
                        if let Some(v) = line.strip_prefix("Content-Length:") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0u8; len];
                    if r.read_exact(&mut body).is_err() {
                        let _ = app.emit("debug-ended", ());
                        return;
                    }
                    let Ok(msg) = serde_json::from_slice::<Value>(&body) else { continue };
                    match msg["type"].as_str() {
                        Some("response") => {
                            if let Some(tx) = msg["request_seq"].as_u64().and_then(|i| pending.lock().unwrap().remove(&i)) {
                                let _ = tx.send(msg);
                            }
                        }
                        Some("event") => match msg["event"].as_str() {
                            Some("initialized") => {
                                let _ = init_tx.send(());
                            }
                            Some("output") => {
                                let cat = msg["body"]["category"].as_str().unwrap_or("console");
                                let text = msg["body"]["output"].as_str().unwrap_or("").to_string();
                                match cat {
                                    "stdout" => console(&app, "out", text),
                                    "stderr" => console(&app, "err", text),
                                    _ => {}
                                }
                            }
                            Some("stopped") => {
                                let tid = msg["body"]["threadId"].as_i64().unwrap_or(1);
                                *thread_id.lock().unwrap() = tid;
                                let reason = msg["body"]["reason"].as_str().unwrap_or("pause").to_string();
                                // Requests from this reader thread would deadlock: ask from another one.
                                let (stdin, pending, seq, source, app) = (stdin.clone(), pending.clone(), seq.clone(), source.clone(), app.clone());
                                thread::spawn(move || match snapshot(&stdin, &pending, &seq, tid, &source, &reason) {
                                    Ok(s) => {
                                        set_state(&app, "running", "Paused (debugger)");
                                        let _ = app.emit("debug-stopped", s);
                                    }
                                    Err(e) => console(&app, "err", format!("debugger: {e}\n")),
                                });
                            }
                            Some("continued") => {
                                let _ = app.emit("debug-continued", ());
                            }
                            Some("exited") => {
                                let code = msg["body"]["exitCode"].as_i64().unwrap_or(0);
                                console(&app, "info", format!("Program exited with code {code}.\n"));
                            }
                            Some("terminated") => {
                                let _ = app.emit("debug-ended", ());
                            }
                            _ => {}
                        },
                        Some("request") => {
                            // Reverse requests (runInTerminal…) are not supported: refuse politely.
                            let _ = send(
                                &stdin,
                                &json!({"type": "response", "seq": 0, "request_seq": msg["seq"], "success": false, "command": msg["command"], "message": "not supported"}),
                            );
                        }
                        _ => {}
                    }
                }
            });
        }

        let session = Session { child, stdin, pending, seq, thread_id, source: source.to_path_buf() };
        session.request(
            "initialize",
            json!({"clientID": "gauthier", "clientName": "Gauthier", "adapterID": "lldb-dap", "linesStartAt1": true, "columnsStartAt1": true, "pathFormat": "path", "supportsVariableType": true}),
        )?;
        let cwd = exe.parent().map(|p| p.display().to_string()).unwrap_or_default();
        // launch answers only after configurationDone with some adapters: send it without waiting.
        let launch_args = json!({
            "program": exe.display().to_string(),
            "args": args,
            "cwd": cwd,
            "stopOnEntry": false,
            "initCommands": rust_lldb_init(),
        });
        let (stdin2, pending2, seq2) = (session.stdin.clone(), session.pending.clone(), session.seq.clone());
        let launch = thread::spawn(move || request_raw(&stdin2, &pending2, &seq2, "launch", launch_args, Duration::from_secs(30)));
        init_rx.recv_timeout(Duration::from_secs(20)).map_err(|_| "the debugger did not start".to_string())?;
        session.request(
            "setBreakpoints",
            json!({"source": {"path": session.source.display().to_string()}, "breakpoints": breakpoints.iter().map(|l| json!({"line": l})).collect::<Vec<_>>()}),
        )?;
        session.request("configurationDone", json!({}))?;
        launch.join().map_err(|_| "launch failed".to_string())??;
        set_state(&app, "running", "Debugging");
        *self.session.lock().unwrap() = Some(session);
        Ok(())
    }

    fn with<T>(&self, f: impl FnOnce(&Session) -> Result<T, String>) -> Result<T, String> {
        let guard = self.session.lock().unwrap();
        let s = guard.as_ref().ok_or("Not debugging")?;
        f(s)
    }

    /// continue / next / stepIn / stepOut
    pub fn step(&self, command: &str) -> Result<(), String> {
        self.with(|s| {
            let tid = *s.thread_id.lock().unwrap();
            s.request(command, json!({"threadId": tid})).map(|_| ())
        })
    }

    pub fn set_breakpoints(&self, lines: &[u32]) -> Result<(), String> {
        self.with(|s| {
            s.request(
                "setBreakpoints",
                json!({"source": {"path": s.source.display().to_string()}, "breakpoints": lines.iter().map(|l| json!({"line": l})).collect::<Vec<_>>()}),
            )
            .map(|_| ())
        })
    }

    pub fn variables(&self, reference: i64) -> Result<Vec<Variable>, String> {
        self.with(|s| fetch_variables(&s.stdin, &s.pending, &s.seq, reference))
    }

    pub fn active(&self) -> bool {
        self.session.lock().unwrap().is_some()
    }

    pub fn stop(&self) {
        if let Some(mut s) = self.session.lock().unwrap().take() {
            let _ = request_raw(&s.stdin, &s.pending, &s.seq, "disconnect", json!({"terminateDebuggee": true}), Duration::from_secs(3));
            let _ = s.child.kill();
            let _ = s.child.wait();
        }
    }
}
