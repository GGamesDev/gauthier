//! Files stored in the Pico's flash (last 512 KiB), like MicroPython's `open()`.
//!
//! The same files are visible from Gauthier's file explorer, over USB, while
//! your program runs.
//!
//! ```ignore
//! use picorust_runtime::fs;
//!
//! fs::write("settings.txt", b"speed=3").await?;
//! let mut buf = [0u8; 64];
//! let n = fs::read("settings.txt", &mut buf).await?;
//! ```
//!
//! Writing erases flash sectors (4 KiB, ~50 ms each, interrupts are off meanwhile):
//! avoid writing in a tight loop.

use embassy_rp::flash::{Blocking, Flash};
use embassy_rp::peripherals::FLASH;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::{Mutex, MutexGuard};

pub use picorust_fs_core::Error;
use picorust_fs_core::{Fs, Storage, SECTOR};

#[cfg(feature = "_rp2040")]
pub(crate) const FLASH_SIZE: usize = 2 * 1024 * 1024;
#[cfg(feature = "_rp235x")]
pub(crate) const FLASH_SIZE: usize = 4 * 1024 * 1024;

/// Size of the file area at the end of flash. `memory.x` must leave it free.
pub const AREA_SIZE: usize = 512 * 1024;
const AREA_OFFSET: u32 = (FLASH_SIZE - AREA_SIZE) as u32;

/// Longest file name, in bytes.
pub const NAME_MAX: usize = picorust_fs_core::NAME_MAX;

pub(crate) struct PicoFlash {
    flash: Flash<'static, FLASH, Blocking, FLASH_SIZE>,
}

impl Storage for PicoFlash {
    fn capacity(&self) -> usize {
        AREA_SIZE
    }
    fn read(&mut self, offset: usize, buf: &mut [u8]) {
        let _ = self.flash.blocking_read(AREA_OFFSET + offset as u32, buf);
    }
    fn erase_sector(&mut self, offset: usize) -> Result<(), Error> {
        let from = AREA_OFFSET + offset as u32;
        self.flash.blocking_erase(from, from + SECTOR as u32).map_err(|_| Error::Flash)
    }
    fn program(&mut self, offset: usize, data: &[u8]) -> Result<(), Error> {
        self.flash.blocking_write(AREA_OFFSET + offset as u32, data).map_err(|_| Error::Flash)
    }
}

pub(crate) type PicoFs = Fs<PicoFlash>;

static FS: Mutex<CriticalSectionRawMutex, Option<PicoFs>> = Mutex::new(None);

/// Mounts the file system (called by `init()`).
pub(crate) fn mount() {
    // SAFETY: picorust-runtime owns the flash peripheral (documented in `init`).
    let flash = unsafe { embassy_rp::peripherals::FLASH::steal() };
    let fs = Fs::mount(PicoFlash { flash: Flash::new_blocking(flash) });
    if let Ok(mut guard) = FS.try_lock() {
        *guard = Some(fs);
    }
}

pub(crate) async fn lock() -> MutexGuard<'static, CriticalSectionRawMutex, Option<PicoFs>> {
    FS.lock().await
}

macro_rules! with_fs {
    ($fs:ident => $body:expr) => {{
        let mut guard = lock().await;
        match guard.as_mut() {
            Some($fs) => $body,
            None => Err(Error::Busy),
        }
    }};
}

/// Reads a whole file into `buf`; returns its length.
pub async fn read(name: &str, buf: &mut [u8]) -> Result<usize, Error> {
    with_fs!(fs => fs.read(name, buf))
}

/// Creates or replaces a file. The old content stays intact until the new one is complete.
pub async fn write(name: &str, data: &[u8]) -> Result<(), Error> {
    with_fs!(fs => fs.write(name, data))
}

pub async fn remove(name: &str) -> Result<(), Error> {
    with_fs!(fs => fs.remove(name))
}

pub async fn rename(from: &str, to: &str) -> Result<(), Error> {
    with_fs!(fs => fs.rename(from, to))
}

/// Size of a file in bytes, or `None` if it does not exist.
pub async fn size(name: &str) -> Option<usize> {
    let guard = lock().await;
    guard.as_ref().and_then(|fs| fs.find(name)).map(|e| e.len as usize)
}

pub async fn exists(name: &str) -> bool {
    size(name).await.is_some()
}

/// Free space in bytes.
pub async fn free_space() -> usize {
    lock().await.as_ref().map_or(0, |fs| fs.free())
}

/// Calls `f(name, size)` for each file.
pub async fn list(mut f: impl FnMut(&str, usize)) {
    if let Some(fs) = lock().await.as_ref() {
        for e in fs.files() {
            f(e.name(), e.len as usize);
        }
    }
}
