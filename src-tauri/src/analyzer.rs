//! Completion and hover through rust-analyzer (Language Server Protocol over stdio).
//! One server per target; it analyses Gauthier's hidden project for that target.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::project::{self, Purpose, Target};
use crate::tools;

type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>;

struct Server {
    target: Target,
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    next_id: u64,
    version: i64,
    uri: String,
    text: String,
}

#[derive(Default)]
pub struct Analyzer {
    server: Mutex<Option<Server>>,
}

#[derive(Serialize, Debug)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    /// LSP CompletionItemKind.
    pub kind: u32,
    pub insert: String,
    /// UTF-16 column where the replaced text starts (when the server says so).
    pub from: Option<u32>,
}

/// file:// URI with percent-encoding (the data folder contains spaces).
fn file_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from(if s.starts_with('/') { "file://" } else { "file:///" });
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn send(stdin: &Mutex<ChildStdin>, msg: &Value) -> Result<(), String> {
    let body = msg.to_string();
    let mut w = stdin.lock().unwrap();
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body).map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())
}

/// Reads framed messages from the server: answers go to their waiters, requests
/// from the server get a neutral answer so it never waits on us.
fn reader(stdout: impl Read + Send + 'static, stdin: Arc<Mutex<ChildStdin>>, pending: Pending) {
    thread::spawn(move || {
        let mut r = BufReader::new(stdout);
        loop {
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    return; // server exited
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
                return;
            }
            let Ok(msg) = serde_json::from_slice::<Value>(&body) else { continue };
            let id = msg.get("id").cloned();
            match (id, msg.get("method")) {
                (Some(id), Some(method)) => {
                    let result = match method.as_str() {
                        Some("workspace/configuration") => {
                            let n = msg["params"]["items"].as_array().map(|a| a.len()).unwrap_or(1);
                            json!(vec![Value::Null; n])
                        }
                        _ => Value::Null,
                    };
                    let _ = send(&stdin, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
                }
                (Some(id), None) => {
                    if let Some(tx) = id.as_u64().and_then(|i| pending.lock().unwrap().remove(&i)) {
                        let _ = tx.send(msg);
                    }
                }
                _ => {} // notifications (progress, diagnostics…) are not used
            }
        }
    });
}

impl Server {
    fn start(target: Target, code: &str, runtime_dir: &Path) -> Result<Server, String> {
        let bin = tools::which("rust-analyzer").ok_or("rust-analyzer is not installed (click \"Install tools\")")?;
        let dir = project::prepare(target, code, runtime_dir, Purpose::Analyzer)?;
        let mut child = tools::command(bin)
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Cannot start rust-analyzer: {e}"))?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let pending: Pending = Arc::default();
        reader(child.stdout.take().unwrap(), stdin.clone(), pending.clone());
        let uri = file_uri(&dir.join(project::MAIN_RS));
        let mut s = Server { target, child, stdin, pending, next_id: 1, version: 1, uri, text: code.to_string() };
        let options = json!({
            "cargo": { "target": target.triple(), "buildScripts": { "enable": true } },
            "checkOnSave": false,
            "procMacro": { "enable": true },
            "completion": { "callable": { "snippets": "none" }, "postfix": { "enable": false } },
        });
        s.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": file_uri(&dir),
                "capabilities": {
                    "textDocument": {
                        "completion": { "completionItem": { "snippetSupport": false } },
                        "hover": { "contentFormat": ["markdown", "plaintext"] }
                    },
                    "general": { "positionEncodings": ["utf-16"] }
                },
                "initializationOptions": options,
            }),
            Duration::from_secs(30),
        )?;
        s.notify("initialized", json!({}))?;
        s.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": s.uri, "languageId": "rust", "version": 1, "text": code } }),
        )?;
        Ok(s)
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        send(&self.stdin, &json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        send(&self.stdin, &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let msg = rx.recv_timeout(timeout).map_err(|_| {
            self.pending.lock().unwrap().remove(&id);
            "rust-analyzer did not answer in time".to_string()
        })?;
        if let Some(err) = msg.get("error") {
            return Err(err["message"].as_str().unwrap_or("rust-analyzer error").to_string());
        }
        Ok(msg["result"].clone())
    }

    fn sync(&mut self, code: &str) -> Result<(), String> {
        if code == self.text {
            return Ok(());
        }
        self.version += 1;
        self.text = code.to_string();
        self.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": self.uri, "version": self.version }, "contentChanges": [{ "text": code }] }),
        )
    }

    /// Requests are retried while the server is still loading ("content modified").
    fn ask(&mut self, method: &str, line: u32, character: u32) -> Result<Value, String> {
        let params = json!({ "textDocument": { "uri": self.uri }, "position": { "line": line, "character": character } });
        let mut last = String::new();
        for _ in 0..3 {
            match self.request(method, params.clone(), Duration::from_secs(10)) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    last = e;
                    thread::sleep(Duration::from_millis(300));
                }
            }
        }
        Err(last)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

impl Analyzer {
    fn with<T>(&self, target: Target, code: &str, runtime_dir: &Path, f: impl FnOnce(&mut Server) -> Result<T, String>) -> Result<T, String> {
        let mut guard = self.server.lock().unwrap();
        let alive = guard.as_mut().map(|s| s.target == target && s.child.try_wait().ok().flatten().is_none()).unwrap_or(false);
        if !alive {
            *guard = None;
            *guard = Some(Server::start(target, code, runtime_dir)?);
        }
        let server = guard.as_mut().unwrap();
        server.sync(code)?;
        f(server)
    }

    /// Starts the server early (indexing takes a few seconds).
    pub fn warm_up(&self, target: Target, code: &str, runtime_dir: &Path) -> Result<(), String> {
        self.with(target, code, runtime_dir, |_| Ok(()))
    }

    pub fn complete(&self, target: Target, code: &str, runtime_dir: &Path, line: u32, character: u32) -> Result<Vec<CompletionItem>, String> {
        self.with(target, code, runtime_dir, |s| {
            let result = s.ask("textDocument/completion", line, character)?;
            let items = match &result {
                Value::Array(a) => a.clone(),
                Value::Object(_) => result["items"].as_array().cloned().unwrap_or_default(),
                _ => Vec::new(),
            };
            Ok(items
                .iter()
                .take(200)
                .map(|it| {
                    let edit = &it["textEdit"];
                    // TextEdit has `range`, InsertReplaceEdit has `insert`.
                    let range = if edit["range"].is_object() { &edit["range"] } else { &edit["insert"] };
                    CompletionItem {
                        label: it["label"].as_str().unwrap_or("").to_string(),
                        detail: it["detail"].as_str().map(str::to_string),
                        kind: it["kind"].as_u64().unwrap_or(1) as u32,
                        insert: edit["newText"]
                            .as_str()
                            .or_else(|| it["insertText"].as_str())
                            .or_else(|| it["label"].as_str())
                            .unwrap_or("")
                            .to_string(),
                        from: range["start"]["character"].as_u64().map(|c| c as u32),
                    }
                })
                .collect())
        })
    }

    pub fn hover(&self, target: Target, code: &str, runtime_dir: &Path, line: u32, character: u32) -> Result<Option<String>, String> {
        self.with(target, code, runtime_dir, |s| {
            let result = s.ask("textDocument/hover", line, character)?;
            let contents = &result["contents"];
            let text = match contents {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o.get("value").and_then(|v| v.as_str()).map(str::to_string),
                Value::Array(a) => Some(
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string).or_else(|| x["value"].as_str().map(str::to_string)))
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                ),
                _ => None,
            };
            Ok(text.filter(|t| !t.trim().is_empty()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::file_uri;
    use std::path::Path;

    #[test]
    fn uri_encodes_spaces() {
        assert_eq!(
            file_uri(Path::new("/Users/a/Library/Application Support/Gauthier/build/main.rs")),
            "file:///Users/a/Library/Application%20Support/Gauthier/build/main.rs"
        );
    }
}

/// Needs rust-analyzer: `cargo test --lib analyzer_live -- --ignored --nocapture`
#[cfg(test)]
mod live {
    use super::*;

    #[test]
    #[ignore]
    fn analyzer_live() {
        let a = Analyzer::default();
        let code = "fn main() {\n    let mut v: Vec<i32> = Vec::new();\n    v.\n}\n";
        let t0 = std::time::Instant::now();
        let mut items = Vec::new();
        // The first answers can be empty while the project is loading.
        for _ in 0..40 {
            items = a.complete(Target::Desktop, code, Path::new("/nonexistent"), 2, 6).unwrap();
            if items.iter().any(|i| i.label.starts_with("push")) {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
        println!("{} items after {:?}; first: {:?}", items.len(), t0.elapsed(), items.iter().take(5).map(|i| &i.label).collect::<Vec<_>>());
        let push = items.iter().find(|i| i.label.starts_with("push")).expect("push not offered");
        println!("push: {push:?}");
        let hover = a.hover(Target::Desktop, code, Path::new("/nonexistent"), 1, 16).unwrap();
        println!("hover Vec: {:?}", hover.as_deref().map(|h| &h[..h.len().min(160)]));
        assert!(hover.unwrap_or_default().contains("Vec"));
    }
}

/// `cargo test --lib analyzer_pico_live -- --ignored --nocapture`
#[cfg(test)]
mod live_pico {
    use super::*;

    #[test]
    #[ignore]
    fn analyzer_pico_live() {
        let a = Analyzer::default();
        let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/picorust-runtime");
        let code = "#![no_std]\n#![no_main]\nuse embassy_executor::Spawner;\nuse embassy_rp::gpio::{Level, Output};\n#[embassy_executor::main]\nasync fn main(spawner: Spawner) {\n    let p = embassy_rp::init(Default::default());\n    picorust_runtime::init(spawner, p.USB).await;\n    let mut led = Output::new(p.PIN_25, Level::Low);\n    led.tog\n}\n";
        let t0 = std::time::Instant::now();
        let mut items = Vec::new();
        for _ in 0..240 {
            items = a.complete(Target::Pico2, code, &runtime, 9, 11).unwrap_or_default();
            if items.iter().any(|i| i.label.starts_with("toggle")) {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
        println!("{} items after {:?}: {:?}", items.len(), t0.elapsed(), items.iter().take(4).map(|i| &i.label).collect::<Vec<_>>());
        assert!(items.iter().any(|i| i.label.starts_with("toggle")), "toggle not offered");
        let fs_code = code.replace("    led.tog\n", "    picorust_runtime::fs::wr\n");
        let fs_items = a.complete(Target::Pico2, &fs_code, &runtime, 9, 28).unwrap_or_default();
        println!("fs:: → {:?}", fs_items.iter().take(4).map(|i| (&i.label, &i.detail)).collect::<Vec<_>>());
        assert!(fs_items.iter().any(|i| i.label == "write"));
    }
}
