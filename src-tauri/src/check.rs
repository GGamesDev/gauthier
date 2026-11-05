//! Checking as you type: `cargo check` in its own project folder.
//! A new request stops the one still running.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::project::{self, Purpose, Target};
use crate::runner::{parse_diagnostic, Diagnostic};
use crate::tools;

#[derive(Default)]
pub struct Checker {
    current: Mutex<Option<Arc<Mutex<Child>>>>,
    generation: AtomicU64,
}

#[derive(Serialize)]
pub struct CheckResult {
    /// False when a newer check replaced this one (ignore the result).
    pub done: bool,
    pub diagnostics: Vec<Diagnostic>,
}

impl Checker {
    pub fn check(&self, code: &str, target: Target, runtime_dir: &Path) -> Result<CheckResult, String> {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(old) = self.current.lock().unwrap().take() {
            let _ = old.lock().unwrap().kill();
        }
        let cargo = tools::which("cargo").ok_or("cargo not found")?;
        let dir = project::prepare(target, code, runtime_dir, Purpose::Check)?;
        let mut cmd = tools::command(cargo);
        cmd.args(["check", "--message-format=json-diagnostic-rendered-ansi", "--quiet"])
            .current_dir(&dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if target != Target::Desktop {
            cmd.arg("--release"); // shares settings with Run; separate folder anyway
        }
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let stdout = child.stdout.take().unwrap();
        let child = Arc::new(Mutex::new(child));
        *self.current.lock().unwrap() = Some(child.clone());

        let mut diagnostics = Vec::new();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(d) = parse_diagnostic(&v) {
                    diagnostics.push(d);
                }
            }
        }
        let _ = child.lock().unwrap().wait();
        let done = self.generation.load(Ordering::SeqCst) == generation;
        if done {
            *self.current.lock().unwrap() = None;
        }
        Ok(CheckResult { done, diagnostics })
    }
}
