mod analyzer;
mod check;
mod debug;
mod pico;
mod project;
mod runner;
mod pico_fs;
mod tools;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};

use project::Target;
use runner::Runner;

/// Folder of the bundled picorust-runtime crate.
fn runtime_dir(app: &AppHandle) -> PathBuf {
    let bundled = app.path().resource_dir().map(|d| d.join("picorust-runtime")).ok();
    match bundled {
        Some(d) if d.join("Cargo.toml").is_file() => d,
        // `tauri dev`: use the source tree.
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources").join("picorust-runtime"),
    }
}

#[tauri::command]
async fn tool_status() -> tools::ToolStatus {
    tauri::async_runtime::spawn_blocking(tools::status).await.unwrap()
}

#[tauri::command]
async fn install_tools(app: AppHandle) -> Result<tools::ToolStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let log = |line: &str| {
            let _ = app.emit("install-log", line.to_string());
        };
        tools::install_missing(&log)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn run_program(app: AppHandle, runner: State<'_, Arc<Runner>>, code: String, name: String, target: Target, args: Option<Vec<String>>) {
    let dir = runtime_dir(&app);
    runner.inner().run(app, code, name, target, args.unwrap_or_default(), dir);
}

#[tauri::command]
async fn list_ports() -> Vec<pico::PortInfo> {
    tauri::async_runtime::spawn_blocking(pico::list_ports).await.unwrap_or_default()
}

#[tauri::command]
async fn connect_serial(app: AppHandle, runner: State<'_, Arc<Runner>>, path: Option<String>, baud: u32) -> Result<(), String> {
    let runner = runner.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runner.connect(app, path, baud)).await.map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------- files on the Pico

macro_rules! blocking {
    ($e:expr) => {
        tauri::async_runtime::spawn_blocking(move || $e).await.map_err(|e| e.to_string())?
    };
}

#[tauri::command]
async fn pico_fs_info(runner: State<'_, Arc<Runner>>) -> Result<pico_fs::PicoFsInfo, String> {
    let fs = runner.files.clone();
    blocking!(fs.info())
}

#[tauri::command]
async fn pico_fs_read_text(runner: State<'_, Arc<Runner>>, name: String) -> Result<String, String> {
    let fs = runner.files.clone();
    let n = name.clone();
    let bytes = blocking!(fs.read(&n))?;
    String::from_utf8(bytes).map_err(|_| format!("{name} is not a text file (download it instead)"))
}

#[tauri::command]
async fn pico_fs_write_text(runner: State<'_, Arc<Runner>>, name: String, contents: String) -> Result<(), String> {
    let fs = runner.files.clone();
    blocking!(fs.write(&name, contents.as_bytes()))
}

#[tauri::command]
async fn pico_fs_delete(runner: State<'_, Arc<Runner>>, name: String) -> Result<(), String> {
    let fs = runner.files.clone();
    blocking!(fs.delete(&name))
}

#[tauri::command]
async fn pico_fs_rename(runner: State<'_, Arc<Runner>>, from: String, to: String) -> Result<(), String> {
    let fs = runner.files.clone();
    blocking!(fs.rename(&from, &to))
}

/// Computer → Pico.
#[tauri::command]
async fn pico_fs_upload(runner: State<'_, Arc<Runner>>, path: String, name: String) -> Result<(), String> {
    let fs = runner.files.clone();
    blocking!({
        let data = std::fs::read(&path).map_err(|e| e.to_string())?;
        fs.write(&name, &data)
    })
}

/// Pico → computer.
#[tauri::command]
async fn pico_fs_download(runner: State<'_, Arc<Runner>>, name: String, path: String) -> Result<(), String> {
    let fs = runner.files.clone();
    blocking!({
        let data = fs.read(&name)?;
        std::fs::write(&path, data).map_err(|e| e.to_string())
    })
}

// ---------------------------------------------------------------- checking & analysis

#[tauri::command]
async fn check_program(app: AppHandle, checker: State<'_, Arc<check::Checker>>, code: String, target: Target) -> Result<check::CheckResult, String> {
    let checker = checker.inner().clone();
    let dir = runtime_dir(&app);
    blocking!(checker.check(&code, target, &dir))
}

#[tauri::command]
async fn analyzer_warm_up(app: AppHandle, analyzer: State<'_, Arc<analyzer::Analyzer>>, code: String, target: Target) -> Result<(), String> {
    let analyzer = analyzer.inner().clone();
    let dir = runtime_dir(&app);
    blocking!(analyzer.warm_up(target, &code, &dir))
}

#[tauri::command]
async fn analyzer_complete(
    app: AppHandle,
    analyzer: State<'_, Arc<analyzer::Analyzer>>,
    code: String,
    target: Target,
    line: u32,
    character: u32,
) -> Result<Vec<analyzer::CompletionItem>, String> {
    let analyzer = analyzer.inner().clone();
    let dir = runtime_dir(&app);
    blocking!(analyzer.complete(target, &code, &dir, line, character))
}

#[tauri::command]
async fn analyzer_hover(
    app: AppHandle,
    analyzer: State<'_, Arc<analyzer::Analyzer>>,
    code: String,
    target: Target,
    line: u32,
    character: u32,
) -> Result<Option<String>, String> {
    let analyzer = analyzer.inner().clone();
    let dir = runtime_dir(&app);
    blocking!(analyzer.hover(target, &code, &dir, line, character))
}

// ---------------------------------------------------------------- debugger

#[tauri::command]
async fn debug_start(
    app: AppHandle,
    runner: State<'_, Arc<Runner>>,
    debugger: State<'_, Arc<debug::Debugger>>,
    code: String,
    args: Option<Vec<String>>,
    breakpoints: Vec<u32>,
) -> Result<bool, String> {
    let (runner, debugger) = (runner.inner().clone(), debugger.inner().clone());
    let dir = runtime_dir(&app);
    blocking!({
        debugger.stop();
        let Some((exe, source)) = runner.build_for_debug(&app, &code, &dir)? else { return Ok(false) };
        if let Err(e) = debugger.start(app.clone(), &exe, &args.unwrap_or_default(), &source, &breakpoints) {
            runner::set_state(&app, "idle", "");
            return Err(e);
        }
        Ok(true)
    })
}

/// continue | next | stepIn | stepOut
#[tauri::command]
async fn debug_step(debugger: State<'_, Arc<debug::Debugger>>, command: String) -> Result<(), String> {
    if !matches!(command.as_str(), "continue" | "next" | "stepIn" | "stepOut") {
        return Err("unknown command".into());
    }
    let debugger = debugger.inner().clone();
    blocking!(debugger.step(&command))
}

#[tauri::command]
async fn debug_breakpoints(debugger: State<'_, Arc<debug::Debugger>>, lines: Vec<u32>) -> Result<(), String> {
    let debugger = debugger.inner().clone();
    blocking!(if debugger.active() { debugger.set_breakpoints(&lines) } else { Ok(()) })
}

#[tauri::command]
async fn debug_variables(debugger: State<'_, Arc<debug::Debugger>>, reference: i64) -> Result<Vec<debug::Variable>, String> {
    let debugger = debugger.inner().clone();
    blocking!(debugger.variables(reference))
}

// ---------------------------------------------------------------- crates

#[derive(serde::Serialize)]
struct CrateInfo {
    name: String,
    version: String,
    description: String,
    downloads: u64,
}

/// Searches crates.io (only the text typed in the Crates window is sent).
#[tauri::command]
async fn crates_search(query: String) -> Result<Vec<CrateInfo>, String> {
    blocking!({
        let mut resp = ureq::get("https://crates.io/api/v1/crates")
            .query("q", &query)
            .query("per_page", "15")
            .header("User-Agent", "Gauthier IDE (https://github.com/GGamesDev/gauthier)")
            .call()
            .map_err(|e| format!("crates.io: {e}"))?;
        let v: serde_json::Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
        Ok(v["crates"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| CrateInfo {
                        name: c["name"].as_str().unwrap_or("").to_string(),
                        version: c["max_stable_version"].as_str().or(c["max_version"].as_str()).unwrap_or("").to_string(),
                        description: c["description"].as_str().unwrap_or("").trim().to_string(),
                        downloads: c["downloads"].as_u64().unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default())
    })
}

// ---------------------------------------------------------------- files on the computer

#[derive(serde::Serialize)]
struct LocalEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: u64,
}

#[tauri::command]
fn default_folder() -> String {
    let dir = dirs::document_dir().unwrap_or_else(std::env::temp_dir).join("Gauthier");
    let _ = std::fs::create_dir_all(&dir);
    dir.display().to_string()
}

#[tauri::command]
fn list_dir(path: String) -> Result<Vec<LocalEntry>, String> {
    let mut out: Vec<LocalEntry> = std::fs::read_dir(&path)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| {
            let meta = e.metadata().ok();
            LocalEntry {
                name: e.file_name().to_string_lossy().to_string(),
                path: e.path().display().to_string(),
                is_dir: meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                size: meta.map(|m| m.len()).unwrap_or(0),
            }
        })
        .collect();
    out.sort_by_key(|e| (!e.is_dir, e.name.to_lowercase()));
    Ok(out)
}

#[tauri::command]
fn parent_dir(path: String) -> Option<String> {
    std::path::Path::new(&path).parent().map(|p| p.display().to_string())
}

#[tauri::command]
fn join_path(dir: String, name: String) -> String {
    std::path::Path::new(&dir).join(name).display().to_string()
}

#[tauri::command]
fn rename_path(from: String, to: String) -> Result<(), String> {
    if std::path::Path::new(&to).exists() {
        return Err(format!("{to} already exists"));
    }
    std::fs::rename(&from, &to).map_err(|e| e.to_string())
}

#[tauri::command]
fn create_dir(path: String) -> Result<(), String> {
    if std::path::Path::new(&path).exists() {
        return Err(format!("{path} already exists"));
    }
    std::fs::create_dir(&path).map_err(|e| e.to_string())
}

/// Moves a file or folder of the computer to the Trash (recoverable), never deletes.
#[tauri::command]
async fn trash_path(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || trash::delete(&path).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn path_exists(path: String) -> bool {
    std::path::Path::new(&path).exists()
}

// Commands that touch processes or serial ports are async: they run off the UI thread.

#[tauri::command]
async fn stop_program(app: AppHandle, runner: State<'_, Arc<Runner>>, debugger: State<'_, Arc<debug::Debugger>>) -> Result<(), String> {
    let runner = runner.inner().clone();
    let debugger = debugger.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        debugger.stop();
        runner.stop();
        if !runner.is_busy() {
            runner::set_state(&app, "idle", "Stopped");
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn send_input(runner: State<'_, Arc<Runner>>, text: String) -> Result<(), String> {
    let runner = runner.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runner.send_input(&text)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn read_text_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

#[tauri::command]
fn write_text_file(path: String, contents: String) -> Result<(), String> {
    std::fs::write(&path, contents).map_err(|e| e.to_string())
}

/// Hidden self-test: GAUTHIER_SELFTEST=desktop|pico|pico2 plays a scripted scenario in the real UI.
#[tauri::command]
fn selftest_mode() -> Option<String> {
    std::env::var("GAUTHIER_SELFTEST").ok().filter(|s| !s.is_empty())
}

#[tauri::command]
fn selftest_report(app: AppHandle, report: String) {
    let path = std::env::var("GAUTHIER_SELFTEST_OUT").unwrap_or_else(|_| "gauthier-selftest.txt".into());
    let _ = std::fs::write(path, report);
    app.exit(0);
}

/// Self-test: a scratch folder next to the report, used as "This computer".
#[tauri::command]
fn selftest_dir() -> Result<String, String> {
    let out = std::env::var("GAUTHIER_SELFTEST_OUT").map_err(|_| "not in self-test".to_string())?;
    let dir = format!("{out}.files");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

#[tauri::command]
fn selftest_window(bounds: String) {
    if let Ok(out) = std::env::var("GAUTHIER_SELFTEST_OUT") {
        let _ = std::fs::write(format!("{out}.window"), bounds);
    }
}

/// Self-test: saves a file next to the report (e.g. the code the scripts must find on the Pico).
#[tauri::command]
fn selftest_save(name: String, contents: String) {
    if let Ok(out) = std::env::var("GAUTHIER_SELFTEST_OUT") {
        let _ = std::fs::write(format!("{out}.{name}"), contents);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(Runner::default()))
        .manage(Arc::new(check::Checker::default()))
        .manage(Arc::new(analyzer::Analyzer::default()))
        .manage(Arc::new(debug::Debugger::default()))
        .invoke_handler(tauri::generate_handler![
            tool_status,
            install_tools,
            run_program,
            list_ports,
            debug_start,
            debug_step,
            debug_breakpoints,
            debug_variables,
            crates_search,
            check_program,
            analyzer_warm_up,
            analyzer_complete,
            analyzer_hover,
            connect_serial,
            pico_fs_info,
            pico_fs_read_text,
            pico_fs_write_text,
            pico_fs_delete,
            pico_fs_rename,
            pico_fs_upload,
            pico_fs_download,
            default_folder,
            list_dir,
            parent_dir,
            join_path,
            path_exists,
            rename_path,
            create_dir,
            trash_path,
            stop_program,
            send_input,
            read_text_file,
            write_text_file,
            selftest_mode,
            selftest_report,
            selftest_window,
            selftest_save,
            selftest_dir
        ])
        .run(tauri::generate_context!())
        .expect("error while running Gauthier");
}
