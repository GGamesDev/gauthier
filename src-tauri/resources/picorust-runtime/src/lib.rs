//! # picorust-runtime
//!
//! Companion crate of the **PicoRust** VS Code extension. It gives a Rust
//! firmware the same "plug and play" workflow as MicroPython:
//!
//! * a USB CDC serial console (`println!`, `print!`, reading from the host);
//! * the **picotool reset interface** and the **1200 baud touch**, so the
//!   extension can reboot the board into BOOTSEL and flash it without pressing
//!   any button;
//! * a panic handler that reboots into a *safe mode*: the panic message is
//!   shown in the console and the board stays flashable.
//!
//! Enable exactly one feature: `embassy-rp2040`, `embassy-rp235x`,
//! `hal-rp2040` or `hal-rp235x`.

#![no_std]

#[cfg(not(any(
    feature = "embassy-rp2040",
    feature = "embassy-rp235x",
    feature = "hal-rp2040",
    feature = "hal-rp235x"
)))]
compile_error!(
    "picorust-runtime: enable one of the features `embassy-rp2040`, `embassy-rp235x`, `hal-rp2040`, `hal-rp235x`"
);

#[cfg(all(feature = "_embassy", feature = "_hal"))]
compile_error!("picorust-runtime: enable only one of the embassy-* / hal-* features");

#[cfg(all(feature = "_rp2040", feature = "_rp235x"))]
compile_error!("picorust-runtime: enable only one chip (rp2040 or rp235x)");

mod reboot;
mod safe_mode;

#[cfg(feature = "_embassy")]
mod embassy;
#[cfg(feature = "_embassy")]
mod files_service;
/// Files in the Pico's flash (Embassy only).
#[cfg(feature = "_embassy")]
pub mod fs;
#[cfg(feature = "_embassy")]
pub use embassy::*;

#[cfg(feature = "_hal")]
mod hal;
#[cfg(feature = "_hal")]
pub use hal::*;

pub use reboot::{reboot, reboot_to_bootsel};
pub use safe_mode::panic_message;

/// USB vendor id (Raspberry Pi). picotool only talks to this vendor id.
pub const USB_VID: u16 = 0x2e8a;
/// USB product id, same as the Pico SDK's `stdio_usb` (so tools recognise it).
pub const USB_PID: u16 = 0x000a;
/// USB product string; the extension uses it to find PicoRust boards.
pub const USB_PRODUCT: &str = "PicoRust";

/// picotool reset interface (see pico-sdk `pico_usb_reset_interface`).
pub(crate) const RESET_CLASS: u8 = 0xff;
pub(crate) const RESET_SUBCLASS: u8 = 0x00;
pub(crate) const RESET_PROTOCOL: u8 = 0x01;
pub(crate) const RESET_REQUEST_BOOTSEL: u8 = 0x01;
pub(crate) const RESET_REQUEST_FLASH: u8 = 0x02;

/// Line coding that asks the firmware to reboot into BOOTSEL ("1200 baud touch").
pub(crate) const MAGIC_BAUD: u32 = 1200;

/// Handles a control OUT request addressed to the reset interface.
/// Never returns for known requests.
pub(crate) fn handle_reset_request(request: u8, value: u16) -> bool {
    match request {
        RESET_REQUEST_BOOTSEL => {
            let mut gpio_mask = 0;
            if value & 0x100 != 0 {
                gpio_mask = 1u32 << (value >> 9);
            }
            reboot::reboot_to_bootsel_with(gpio_mask, u32::from(value & 0x7f))
        }
        RESET_REQUEST_FLASH => reboot::reboot(),
        _ => false,
    }
}

/// `fmt::Write` sink that goes to the USB console.
#[doc(hidden)]
pub struct Console;

impl core::fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write(s.as_bytes());
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    let _ = core::fmt::Write::write_fmt(&mut Console, args);
}

/// Prints to the USB console (dropped when no terminal is connected).
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::_print(core::format_args!($($arg)*)) };
}

/// Prints to the USB console, with a newline.
#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        $crate::_print(core::format_args!($($arg)*));
        $crate::print!("\n");
    }};
}
