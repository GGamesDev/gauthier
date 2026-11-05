<p align="center">
  <img src="src-tauri/icons/128x128@2x.png" width="128" alt="Gauthier logo">
</p>

<h1 align="center">Gauthier</h1>

<p align="center">A simple Rust IDE for your computer and the Raspberry Pi Pico — think Thonny, but for Rust.</p>

---

Gauthier is a small desktop editor for learning and tinkering with Rust. Write a program, press **Run**, see the
output. Pick a Raspberry Pi Pico as the target and the same button builds, flashes and opens the board's console —
no BOOTSEL button, no copying `.uf2` files around.

## Features

**Editing**
- Tabs, restored with their unsaved changes when you reopen the app
- Completion and documentation on hover (rust-analyzer)
- Errors underlined while you type, with a plain-words explanation and a hint for the most common ones
- Outline of the file (functions, structs, impls…), recent files, zoom, light/dark theme

**Running**
- One button: build and run on the computer, or on a Pico / Pico 2
- Interactive console (your program can read what you type), program arguments
- Plotter: numbers printed by your program (`temp: 21.5`) are drawn as live curves
- Step-by-step debugger with breakpoints, call stack and variables (programs on the computer)
- Crates from crates.io, declared at the top of the file and added from a search window

**Raspberry Pi Pico**
- Flashing without BOOTSEL: the board is rebooted into its bootloader over USB
- Serial monitor, with automatic reconnection and any port / baud rate
- Files on the board, like MicroPython: a file explorer shows them next to your computer's files. They live in the
  last 512 KB of flash and are read and written over USB **while your program runs**. Every Run keeps the program's
  source as `main.rs` on the board.
- Your program can use the same files: `picorust_runtime::fs::read("config.txt", &mut buf).await`
- A panic reboots the board into a safe mode that shows the message and stays flashable

**Setup**
- On first launch, Gauthier installs what is missing: Rust (rustup), the Pico targets, picotool and rust-analyzer

## Install

Download `Gauthier_<version>_aarch64.dmg` from the [releases page](https://github.com/GGamesDev/gauthier/releases),
open it and drag Gauthier to *Applications*.

- Prebuilt builds are for **Apple Silicon Macs** (M1 and later). On other systems, build from source (below).
- The app is not notarized by Apple yet. The first time, macOS refuses to open it: right-click Gauthier →
  **Open**, or go to *System Settings → Privacy & Security* and click **Open Anyway**. Only needed once.

## Getting started

Build it yourself:

```bash
npm install
npm run tauri dev      # development
npm run tauri build    # app bundle in src-tauri/target/release/bundle
```

Building requires [Rust](https://rustup.rs) and Node.js 20+. On macOS, the Command Line Tools
(`xcode-select --install`) provide the linker and the debugger (`lldb-dap`).

The very first time you use a Pico (new board, or one running MicroPython / C), hold **BOOTSEL** while plugging it
in. After that, Gauthier handles it. On macOS, accept the *"Allow accessory to connect?"* prompt when it appears.

### Pico programs

Pico programs use [Embassy](https://embassy.dev). Keep the `picorust_runtime::init(...)` line at the start of
`main` — it provides the USB console, the file service and the remote reboot:

```rust
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_time::Timer;
use picorust_runtime::println;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    picorust_runtime::init(spawner, p.USB).await;

    let mut led = Output::new(p.PIN_25, Level::Low);
    loop {
        led.toggle();
        println!("Blink!");
        Timer::after_millis(500).await;
    }
}
```

### Crates

Dependencies are written at the top of the file (same format as `rust-script`); the **Crates** button edits them:

```rust
//! ```cargo
//! [dependencies]
//! rand = "0.9"
//! ```
```

## Project layout

| Path | |
|---|---|
| `src/` | User interface (TypeScript, CodeMirror) |
| `src-tauri/src/` | Backend: builds, runs, Pico flashing and files, rust-analyzer and lldb-dap clients |
| `src-tauri/resources/picorust-runtime/` | Crate linked into Pico programs: USB console, reset interface, file system |
| `src-tauri/resources/picorust-runtime/fs-core/` | The flash file system itself (`no_std`, power-cut safe, host-tested) |
| `scripts/` | Hardware test scripts |

## Tests

```bash
(cd src-tauri && cargo test --lib)
(cd src-tauri/resources/picorust-runtime/fs-core && cargo test)   # file system, including power cuts
scripts/test-pico.sh pico2                                         # end to end, with a Pico 2 plugged in
scripts/pico_files.py list                                         # files on a Pico, read with picotool only
```

The app has a scripted self-test mode that drives the real UI (`GAUTHIER_SELFTEST=desktop`, or `pico2-files` with a
board plugged in); `test-pico.sh` uses it and then checks the board's flash independently.

## License

MIT — see [LICENSE](LICENSE).
