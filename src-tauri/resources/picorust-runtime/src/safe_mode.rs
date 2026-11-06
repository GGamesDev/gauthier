//! Panic "safe mode".
//!
//! On panic the message is saved in a RAM area that is not initialised at
//! boot, then the chip reboots. At the next boot `init()` finds the message,
//! starts USB only (console + reset interface) and prints the message whenever
//! a terminal connects, instead of running the user firmware again. The board
//! therefore stays flashable from the extension without pressing BOOTSEL.

use core::mem::MaybeUninit;

const MAGIC: u32 = 0x5049_4352; // "PICR"
pub(crate) const MSG_CAP: usize = 512;

#[repr(C)]
struct Record {
    magic: u32,
    len: u32,
    check: u32,
    msg: [u8; MSG_CAP],
}

#[link_section = ".uninit.picorust_panic"]
static mut RECORD: MaybeUninit<Record> = MaybeUninit::uninit();

fn checksum(len: u32, msg: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5 ^ len;
    for &b in msg {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    h
}

/// Copy of the panic message found at boot (`None` in normal mode).
static mut FOUND: Option<([u8; MSG_CAP], usize)> = None;

/// Reads (and clears) the record left by a previous panic. Called once by `init()`.
pub(crate) fn take_at_boot() -> bool {
    unsafe {
        let rec = &mut *core::ptr::addr_of_mut!(RECORD).cast::<Record>();
        let magic = core::ptr::read_volatile(&rec.magic);
        let len = core::ptr::read_volatile(&rec.len) as usize;
        if magic != MAGIC || len > MSG_CAP {
            return false;
        }
        let ok = checksum(len as u32, &rec.msg[..len]) == core::ptr::read_volatile(&rec.check);
        core::ptr::write_volatile(&mut rec.magic, 0);
        if !ok {
            return false;
        }
        let mut copy = [0u8; MSG_CAP];
        copy[..len].copy_from_slice(&rec.msg[..len]);
        *core::ptr::addr_of_mut!(FOUND) = Some((copy, len));
        true
    }
}

/// The panic message of the previous run, if the board is in safe mode.
pub fn panic_message() -> Option<&'static [u8]> {
    unsafe {
        (*core::ptr::addr_of!(FOUND))
            .as_ref()
            .map(|(buf, len)| &buf[..*len])
    }
}

/// Banner printed in safe mode each time a terminal connects.
pub(crate) fn write_banner(mut out: impl FnMut(&[u8])) {
    if let Some(msg) = panic_message() {
        out(b"\r\n\x1b[31m*** PicoRust: the firmware panicked ***\x1b[0m\r\n");
        out(msg);
        out(b"\r\n\x1b[33m(safe mode: your program is stopped; run it again to restart it)\x1b[0m\r\n");
    }
}

#[cfg(feature = "panic-handler")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;

    struct Buf<'a> {
        buf: &'a mut [u8; MSG_CAP],
        len: usize,
    }
    impl Write for Buf<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for &b in s.as_bytes() {
                if self.len >= MSG_CAP {
                    break;
                }
                // Terminal friendly newlines.
                if b == b'\n' && self.len + 1 < MSG_CAP {
                    self.buf[self.len] = b'\r';
                    self.len += 1;
                }
                self.buf[self.len] = b;
                self.len += 1;
            }
            Ok(())
        }
    }

    cortex_m::interrupt::disable();
    unsafe {
        let rec = &mut *core::ptr::addr_of_mut!(RECORD).cast::<Record>();
        let mut w = Buf { buf: &mut rec.msg, len: 0 };
        let _ = write!(w, "{}", info);
        let len = w.len;
        rec.len = len as u32;
        rec.check = checksum(len as u32, &rec.msg[..len]);
        core::ptr::write_volatile(&mut rec.magic, MAGIC);
    }
    cortex_m::asm::dsb();
    crate::reboot::reboot()
}
