//! The hidden Cargo project Gauthier builds the user's file in.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tools;

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    Desktop,
    Pico,
    Pico2,
}

impl Target {
    pub fn triple(self) -> Option<&'static str> {
        match self {
            Target::Desktop => None,
            Target::Pico => Some("thumbv6m-none-eabi"),
            Target::Pico2 => Some("thumbv8m.main-none-eabihf"),
        }
    }
    fn dir_name(self) -> &'static str {
        match self {
            Target::Desktop => "desktop",
            Target::Pico => "pico",
            Target::Pico2 => "pico2",
        }
    }
}

pub const BIN: &str = "program";
/// Path of the user's code inside the project, as rustc reports it.
pub const MAIN_RS: &str = "src/main.rs";

/// Each use has its own project folder, so a Run, a live check and the code
/// analyzer never wait for each other's Cargo lock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Purpose {
    Build,
    Check,
    Analyzer,
}

pub fn project_dir(target: Target) -> PathBuf {
    project_dir_for(target, Purpose::Build)
}

pub fn project_dir_for(target: Target, purpose: Purpose) -> PathBuf {
    let suffix = match purpose {
        Purpose::Build => "",
        Purpose::Check => "-check",
        Purpose::Analyzer => "-analyzer",
    };
    tools::data_dir().join("build").join(format!("{}{suffix}", target.dir_name()))
}

/// Writes (or refreshes) the project and the user's code. Returns the project folder.
/// Dependencies written at the top of the file, in a `cargo` block (same format as rust-script):
///
/// ```text
/// //! ```cargo
/// //! [dependencies]
/// //! rand = "0.9"
/// //! ```
/// ```
pub fn file_dependencies(code: &str) -> Vec<String> {
    let mut deps = Vec::new();
    let mut in_block = false;
    let mut in_deps = false;
    for line in code.lines() {
        let Some(rest) = line.trim_start().strip_prefix("//!") else {
            let t = line.trim_start();
            // Before the block: blank lines, comments and #![attributes] are fine.
            if in_block || !(t.is_empty() || t.starts_with("//") || t.starts_with("#![")) {
                break;
            }
            continue;
        };
        let rest = rest.trim();
        if rest.starts_with("```") {
            if in_block {
                break;
            }
            in_block = rest.trim_start_matches('`').trim() == "cargo";
            continue;
        }
        if !in_block {
            continue;
        }
        if rest.starts_with('[') {
            in_deps = rest == "[dependencies]";
            continue;
        }
        let valid_name = rest.split('=').next().map(|n| {
            let n = n.trim();
            !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
        if in_deps && rest.contains('=') && valid_name == Some(true) {
            deps.push(rest.to_string());
        }
    }
    deps
}

pub fn prepare(target: Target, code: &str, runtime_dir: &Path, purpose: Purpose) -> Result<PathBuf, String> {
    let dir = project_dir_for(target, purpose);
    let extra = file_dependencies(code).join("\n");
    let files: Vec<(&str, String)> = match target {
        Target::Desktop => vec![("Cargo.toml", format!("{}{extra}\n", desktop_cargo_toml()))],
        Target::Pico | Target::Pico2 => vec![
            ("Cargo.toml", pico_cargo_toml(target, runtime_dir).replacen("[profile.release]", &format!("{extra}\n\n[profile.release]"), 1)),
            (".cargo/config.toml", pico_cargo_config(target)),
            ("memory.x", pico_memory_x(target)),
            ("build.rs", PICO_BUILD_RS.to_string()),
        ],
    };
    for (rel, content) in files {
        write_if_changed(&dir.join(rel), &content)?;
    }
    write_if_changed(&dir.join(MAIN_RS), code)?;
    Ok(dir)
}

/// Unchanged files keep their timestamp, so Cargo does not rebuild for nothing.
fn write_if_changed(path: &Path, content: &str) -> Result<(), String> {
    if fs::read_to_string(path).ok().as_deref() == Some(content) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(path, content).map_err(|e| e.to_string())
}

pub fn elf_path(target: Target) -> PathBuf {
    let dir = project_dir(target).join("target");
    match target.triple() {
        Some(t) => dir.join(t).join("release").join(BIN),
        None => dir.join("debug").join(if cfg!(windows) { "program.exe" } else { BIN }),
    }
}

fn desktop_cargo_toml() -> String {
    format!(
        r#"[package]
name = "{BIN}"
version = "0.1.0"
edition = "2024"

[dependencies]
"#
    )
}

fn pico_cargo_toml(target: Target, runtime_dir: &Path) -> String {
    let (chip, runtime_feature, extra) = match target {
        Target::Pico => ("rp2040", "embassy-rp2040", ""),
        _ => ("rp235xa", "embassy-rp235x", ", \"binary-info\""),
    };
    // TOML basic string: escape backslashes (Windows paths).
    let runtime = runtime_dir.display().to_string().replace('\\', "\\\\");
    format!(
        r#"[package]
name = "{BIN}"
version = "0.1.0"
edition = "2024"

[dependencies]
picorust-runtime = {{ path = "{runtime}", features = ["{runtime_feature}"] }}
embassy-executor = {{ version = "0.10", features = ["platform-cortex-m", "executor-thread", "executor-interrupt"] }}
embassy-rp = {{ version = "0.10", features = ["{chip}", "time-driver", "critical-section-impl"{extra}] }}
embassy-time = "0.5"
embassy-sync = "0.8"
embassy-futures = "0.1"
cortex-m = "0.7"
cortex-m-rt = "0.7"
static_cell = "2.1"

[profile.release]
opt-level = "s"
lto = "fat"
codegen-units = 1
debug = 2
incremental = false
"#
    )
}

fn pico_cargo_config(target: Target) -> String {
    let triple = target.triple().unwrap();
    let mut flags = vec!["--nmagic", "-Tlink.x"];
    if target == Target::Pico {
        flags.push("-Tlink-rp.x");
    }
    let flags = flags.iter().map(|f| format!("\"-C\", \"link-arg={f}\"")).collect::<Vec<_>>().join(", ");
    format!(
        r#"[build]
target = "{triple}"

[target.'cfg(all(target_arch = "arm", target_os = "none"))']
rustflags = [{flags}]
"#
    )
}

fn pico_memory_x(target: Target) -> String {
    match target {
        Target::Pico => "MEMORY {
    BOOT2 : ORIGIN = 0x10000000, LENGTH = 0x100
    FLASH : ORIGIN = 0x10000100, LENGTH = 2048K - 0x100 - 512K  /* last 512K: files (picorust_runtime::fs) */
    RAM   : ORIGIN = 0x20000000, LENGTH = 256K
}
"
        .into(),
        _ => "MEMORY {
    FLASH : ORIGIN = 0x10000000, LENGTH = 4096K - 512K  /* last 512K: files (picorust_runtime::fs) */
    RAM   : ORIGIN = 0x20000000, LENGTH = 512K
    SRAM4 : ORIGIN = 0x20080000, LENGTH = 4K
    SRAM5 : ORIGIN = 0x20081000, LENGTH = 4K
}

SECTIONS {
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;
        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH
} INSERT AFTER .vector_table;

_stext = ALIGN(ADDR(.start_block) + SIZEOF(.start_block), 8);

SECTIONS {
    .bi_entries : ALIGN(4)
    {
        __bi_entries_start = .;
        KEEP(*(.bi_entries));
        . = ALIGN(4);
        __bi_entries_end = .;
    } > FLASH
} INSERT AFTER .text;

SECTIONS {
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
    } > FLASH
} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);
"
        .into(),
    }
}

const PICO_BUILD_RS: &str = r#"use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("memory.x"), include_bytes!("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
}
"#;

#[cfg(test)]
mod tests {
    use super::file_dependencies;

    #[test]
    fn reads_the_cargo_block() {
        let code = "//! ```cargo\n//! [dependencies]\n//! rand = \"0.9\"\n//! serde = { version = \"1\", features = [\"derive\"] }\n//! ```\n\nfn main() {}\n";
        assert_eq!(file_dependencies(code), vec!["rand = \"0.9\"", "serde = { version = \"1\", features = [\"derive\"] }"]);
    }

    #[test]
    fn ignores_other_comments_and_code() {
        assert!(file_dependencies("// hello\nfn main() { let rand = 3; }\n").is_empty());
        assert!(file_dependencies("fn main() {}\n//! ```cargo\n//! [dependencies]\n//! x = \"1\"\n//! ```\n").is_empty());
        assert!(file_dependencies("//! ```cargo\n//! [package]\n//! name = \"x\"\n//! ```\n").is_empty());
    }

    #[test]
    fn block_after_no_std() {
        let code = "#![no_std]\n#![no_main]\n//! ```cargo\n//! [dependencies]\n//! heapless = \"0.9\"\n//! ```\nuse x;\n";
        assert_eq!(file_dependencies(code), vec!["heapless = \"0.9\""]);
    }

    #[test]
    fn after_inner_attributes_comments() {
        let code = "// My program\n//! ```cargo\n//! [dependencies]\n//! heapless = \"0.9\"\n//! ```\n#![no_std]\n";
        assert_eq!(file_dependencies(code), vec!["heapless = \"0.9\""]);
    }
}
