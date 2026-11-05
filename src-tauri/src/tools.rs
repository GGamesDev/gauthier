//! Finding and installing the external tools: rustup/cargo, Rust targets, picotool.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

pub const PICO_TARGETS: [&str; 2] = ["thumbv6m-none-eabi", "thumbv8m.main-none-eabihf"];
const PICOTOOL_VERSION: &str = "2.3.1";

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

/// ~/Library/Application Support/Gauthier (macOS), %LOCALAPPDATA%\Gauthier, ~/.local/share/Gauthier.
pub fn data_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(env::temp_dir).join("Gauthier")
}

fn tools_dir() -> PathBuf {
    data_dir().join("tools")
}

/// Folders searched in addition to PATH: apps started from the Dock/Start menu have a minimal PATH.
fn extra_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".cargo").join("bin"));
    }
    dirs.push(tools_dir().join(format!("picotool-{PICOTOOL_VERSION}")).join("picotool"));
    if !cfg!(windows) {
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
    }
    dirs
}

/// PATH used for every child process.
pub fn tool_path() -> OsString {
    let mut parts: Vec<PathBuf> = extra_dirs();
    if let Some(p) = env::var_os("PATH") {
        parts.extend(env::split_paths(&p));
    }
    env::join_paths(parts).unwrap_or_default()
}

pub fn which(name: &str) -> Option<PathBuf> {
    env::split_paths(&tool_path())
        .map(|d| d.join(exe(name)))
        .find(|p| p.is_file())
}

/// A `Command` with the extended PATH.
pub fn command(program: impl AsRef<Path>) -> Command {
    let mut c = Command::new(program.as_ref());
    c.env("PATH", tool_path());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn output(program: &Path, args: &[&str]) -> Option<String> {
    let out = command(program).args(args).output().ok()?;
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    out.status.success().then_some(s)
}

#[derive(Serialize, Clone, Debug)]
pub struct ToolStatus {
    pub cargo: Option<String>,
    /// macOS: the Command Line Tools provide the linker used for programs on the computer.
    pub linker: bool,
    pub missing_targets: Vec<String>,
    pub picotool: Option<String>,
    /// rust-analyzer (completion, hover) and the standard library sources it needs.
    pub analyzer: bool,
}

impl ToolStatus {
    pub fn desktop_ready(&self) -> bool {
        self.cargo.is_some() && self.linker
    }
    pub fn pico_ready(&self) -> bool {
        self.cargo.is_some() && self.missing_targets.is_empty() && self.picotool.is_some()
    }
}

pub fn status() -> ToolStatus {
    let cargo = which("cargo").and_then(|c| output(&c, &["--version"])).map(|s| s.trim().to_string());
    let linker = if cfg!(target_os = "macos") {
        Command::new("xcode-select").arg("-p").output().map(|o| o.status.success()).unwrap_or(false)
    } else {
        true
    };
    let missing_targets = match which("rustup").and_then(|r| output(&r, &["target", "list", "--installed"])) {
        Some(list) => PICO_TARGETS
            .iter()
            .filter(|t| !list.lines().any(|l| l.trim() == **t))
            .map(|t| t.to_string())
            .collect(),
        None => PICO_TARGETS.iter().map(|t| t.to_string()).collect(),
    };
    let picotool = which("picotool")
        .and_then(|p| output(&p, &["version"]))
        .and_then(|s| s.split_whitespace().find(|w| w.starts_with('v')).map(str::to_string))
        .filter(|v| v.trim_start_matches('v').split('.').next().and_then(|m| m.parse::<u32>().ok()).unwrap_or(0) >= 2);
    let analyzer = which("rust-analyzer").and_then(|ra| output(&ra, &["--version"])).is_some()
        && which("rustup")
            .and_then(|r| output(&r, &["component", "list", "--installed"]))
            .map(|l| l.lines().any(|x| x.starts_with("rust-src")))
            .unwrap_or(false);
    ToolStatus { cargo, linker, missing_targets, picotool, analyzer }
}

/// Runs a command and forwards each output line to `log`.
pub fn run_logged(mut cmd: Command, log: &dyn Fn(&str)) -> Result<(), String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        BufReader::new(stderr).lines().map_while(Result::ok).collect::<Vec<_>>()
    });
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        log(&line);
    }
    for line in err_thread.join().unwrap_or_default() {
        log(&line);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("exited with {status}")) }
}

fn download(url: &str, dest: &Path) -> Result<(), String> {
    let mut resp = ureq::get(url).call().map_err(|e| format!("download {url}: {e}"))?;
    let mut file = fs::File::create(dest).map_err(|e| e.to_string())?;
    std::io::copy(&mut resp.body_mut().as_reader(), &mut file).map_err(|e| e.to_string())?;
    Ok(())
}

fn host_triple() -> Option<&'static str> {
    Some(match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        // The GNU toolchain ships its own linker: no Visual Studio needed.
        ("windows", "x86_64") | ("windows", "aarch64") => "x86_64-pc-windows-gnu",
        _ => return None,
    })
}

/// Installs whatever is missing. `log` receives progress lines.
pub fn install_missing(log: &dyn Fn(&str)) -> Result<ToolStatus, String> {
    let mut st = status();

    if !st.linker {
        log("Installing Apple's Command Line Tools: follow the macOS dialog, then click Install again.");
        let _ = Command::new("xcode-select").arg("--install").status();
        return Err("Waiting for the Command Line Tools installation".into());
    }

    if st.cargo.is_none() {
        let triple = host_triple().ok_or("No prebuilt Rust for this computer")?;
        log("Downloading the Rust installer (rustup)…");
        let dir = data_dir().join("downloads");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let installer = dir.join(exe("rustup-init"));
        download(&format!("https://static.rust-lang.org/rustup/dist/{triple}/{}", exe("rustup-init")), &installer)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&installer, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
        log("Installing Rust (this takes a few minutes)…");
        let mut cmd = command(&installer);
        cmd.args(["-y", "--profile", "minimal", "--no-modify-path"]);
        if cfg!(windows) {
            cmd.args(["--default-host", triple]);
        }
        run_logged(cmd, log)?;
        st = status();
    }

    if !st.missing_targets.is_empty() {
        let rustup = which("rustup").ok_or("rustup not found")?;
        log(&format!("Adding Rust targets for the Pico: {}…", st.missing_targets.join(", ")));
        let mut cmd = command(rustup);
        cmd.args(["target", "add"]).args(&st.missing_targets);
        run_logged(cmd, log)?;
    }

    if st.picotool.is_none() {
        install_picotool(log)?;
    }

    if !st.analyzer {
        let rustup = which("rustup").ok_or("rustup not found")?;
        log("Adding rust-analyzer (completion and help on hover)…");
        let mut cmd = command(rustup);
        cmd.args(["component", "add", "rust-analyzer", "rust-src"]);
        run_logged(cmd, log)?;
    }

    let st = status();
    log(if st.pico_ready() { "All tools are ready." } else { "Some tools are still missing." });
    Ok(st)
}

fn install_picotool(log: &dyn Fn(&str)) -> Result<(), String> {
    let asset = match (env::consts::OS, env::consts::ARCH) {
        ("macos", _) => format!("picotool-{PICOTOOL_VERSION}-mac.zip"),
        ("windows", _) => format!("picotool-{PICOTOOL_VERSION}-x64-win.zip"),
        ("linux", "x86_64") => format!("picotool-{PICOTOOL_VERSION}-x86_64-lin.tar.gz"),
        ("linux", "aarch64") => format!("picotool-{PICOTOOL_VERSION}-aarch64-lin.tar.gz"),
        _ => return Err("No prebuilt picotool for this computer".into()),
    };
    log(&format!("Downloading picotool {PICOTOOL_VERSION}…"));
    let dest = tools_dir().join(format!("picotool-{PICOTOOL_VERSION}"));
    fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let archive = dest.join(&asset);
    download(
        &format!("https://github.com/raspberrypi/pico-sdk-tools/releases/download/v{PICOTOOL_VERSION}-0/{asset}"),
        &archive,
    )?;
    // bsdtar (macOS, Windows 10+) and GNU tar both extract these archives.
    let ok = command("tar").arg("-xf").arg(&archive).arg("-C").arg(&dest).status().map(|s| s.success()).unwrap_or(false);
    let _ = fs::remove_file(&archive);
    if !ok {
        return Err("Could not extract picotool".into());
    }
    log("picotool installed.");
    Ok(())
}
