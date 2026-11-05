//! rp-hal (rp2040-hal / rp235x-hal + usb-device) implementation.
//! USB is serviced from the `USBCTRL_IRQ` interrupt, so no polling is needed.

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use critical_section::Mutex;
use usb_device::class_prelude::*;
use usb_device::prelude::*;
use usbd_serial::SerialPort;

#[cfg(feature = "hal-rp2040")]
use rp2040_hal as rphal;
#[cfg(feature = "hal-rp235x")]
use rp235x_hal as rphal;

use rphal::pac::{self, interrupt};
pub use rphal::usb::UsbBus;

use crate::safe_mode;

struct Ring<const N: usize> {
    buf: [u8; N],
    head: usize,
    len: usize,
}

impl<const N: usize> Ring<N> {
    const fn new() -> Self {
        Self { buf: [0; N], head: 0, len: 0 }
    }
    /// Returns false when some bytes did not fit.
    fn push(&mut self, data: &[u8]) -> bool {
        for &b in data {
            if self.len == N {
                return false;
            }
            self.buf[(self.head + self.len) % N] = b;
            self.len += 1;
        }
        true
    }
    /// Longest contiguous readable slice.
    fn front(&self) -> &[u8] {
        let end = core::cmp::min(self.head + self.len, N);
        &self.buf[self.head..end]
    }
    fn consume(&mut self, n: usize) {
        self.head = (self.head + n) % N;
        self.len -= n;
    }
    fn pop_into(&mut self, out: &mut [u8]) -> usize {
        let mut n = 0;
        while n < out.len() && self.len > 0 {
            out[n] = self.buf[self.head];
            self.consume(1);
            n += 1;
        }
        n
    }
    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

struct Usb {
    dev: UsbDevice<'static, UsbBus>,
    serial: SerialPort<'static, UsbBus>,
    reset: ResetClass,
}

static USB: Mutex<RefCell<Option<Usb>>> = Mutex::new(RefCell::new(None));
static TX: Mutex<RefCell<Ring<2048>>> = Mutex::new(RefCell::new(Ring::new()));
static RX: Mutex<RefCell<Ring<512>>> = Mutex::new(RefCell::new(Ring::new()));
static CONNECTED: AtomicBool = AtomicBool::new(false);
/// Set when output had to be dropped because the buffer was full.
static OVERFLOW: AtomicBool = AtomicBool::new(false);
/// Incremented each time a terminal opens the port (single writer: the USB control path).
static SESSIONS: AtomicU32 = AtomicU32::new(0);
static mut ALLOC: Option<UsbBusAllocator<UsbBus>> = None;

/// Starts the USB console and the reset interface.
///
/// ```ignore
/// let bus = UsbBus::new(pac.USBCTRL_REGS, pac.USBCTRL_DPRAM, clocks.usb_clock, true, &mut pac.RESETS);
/// picorust_runtime::init(bus);
/// ```
///
/// Must be called once. If the previous run panicked, it never returns (safe mode).
pub fn init(bus: UsbBus) {
    let safe = safe_mode::take_at_boot();

    let alloc: &'static UsbBusAllocator<UsbBus> = unsafe {
        let slot = &mut *core::ptr::addr_of_mut!(ALLOC);
        assert!(slot.is_none(), "picorust_runtime::init called twice");
        slot.insert(UsbBusAllocator::new(bus))
    };

    let serial = SerialPort::new(alloc);
    let reset = ResetClass { iface: alloc.interface() };
    let dev = UsbDeviceBuilder::new(alloc, UsbVidPid(crate::USB_VID, crate::USB_PID))
        .strings(&[StringDescriptors::default()
            .manufacturer("PicoRust")
            .product(crate::USB_PRODUCT)
            // Same descriptors in safe mode: macOS asks to approve every "new" accessory.
            .serial_number("picorust")])
        .unwrap()
        .composite_with_iads()
        .max_packet_size_0(64)
        .unwrap()
        .max_power(100)
        .unwrap()
        .build();

    critical_section::with(|cs| {
        USB.borrow_ref_mut(cs).replace(Usb { dev, serial, reset });
    });
    unsafe { cortex_m::peripheral::NVIC::unmask(pac::Interrupt::USBCTRL_IRQ) };

    if safe {
        // Safe mode: the user firmware does not run. Show the panic message to
        // every terminal that connects (after a short delay: the host may flush
        // its input buffer right after opening the port).
        let mut shown = 0;
        loop {
            let session = SESSIONS.load(Ordering::Relaxed);
            if session != shown && is_connected() {
                cortex_m::asm::delay(45_000_000); // ~300 ms at 125-150 MHz
                write_banner_now();
                shown = session;
            }
            cortex_m::asm::wfi();
        }
    }
}

fn write_banner_now() {
    safe_mode::write_banner(write);
}

/// True when a terminal has the serial port open (DTR set).
pub fn is_connected() -> bool {
    CONNECTED.load(Ordering::Relaxed)
}

/// Queues bytes for the USB console. Never blocks. Output written before a
/// terminal connects is kept (up to the buffer size) and shown when it connects.
pub fn write(bytes: &[u8]) {
    critical_section::with(|cs| {
        if !TX.borrow_ref_mut(cs).push(bytes) {
            OVERFLOW.store(true, Ordering::Relaxed);
        }
    });
    cortex_m::peripheral::NVIC::pend(pac::Interrupt::USBCTRL_IRQ);
}

/// Non-blocking read of bytes typed in the VS Code terminal; returns 0 when nothing is available.
pub fn try_read(buf: &mut [u8]) -> usize {
    critical_section::with(|cs| RX.borrow_ref_mut(cs).pop_into(buf))
}

/// Blocking read of one line typed in the VS Code terminal (without the line ending).
pub fn read_line(buf: &mut [u8]) -> usize {
    let mut len = 0;
    loop {
        let mut b = [0u8; 1];
        if try_read(&mut b) == 0 {
            cortex_m::asm::wfi();
            continue;
        }
        match b[0] {
            b'\r' | b'\n' => {
                if len > 0 {
                    return len;
                }
            }
            c => {
                if len < buf.len() {
                    buf[len] = c;
                    len += 1;
                }
            }
        }
    }
}

fn service() {
    critical_section::with(|cs| {
        let mut usb = USB.borrow_ref_mut(cs);
        let Some(usb) = usb.as_mut() else { return };

        if usb.dev.poll(&mut [&mut usb.serial, &mut usb.reset]) {
            let mut buf = [0u8; 64];
            if let Ok(n) = usb.serial.read(&mut buf) {
                let _ = RX.borrow_ref_mut(cs).push(&buf[..n]);
            }
        }

        if usb.serial.line_coding().data_rate() == crate::MAGIC_BAUD {
            crate::reboot::reboot_to_bootsel();
        }

        let dtr = usb.serial.dtr() && usb.dev.state() == UsbDeviceState::Configured;
        // load + store (no CAS on Cortex-M0+); this is the only writer.
        let was = CONNECTED.load(Ordering::Relaxed);
        CONNECTED.store(dtr, Ordering::Relaxed);
        let mut tx = TX.borrow_ref_mut(cs);
        if dtr && !was && OVERFLOW.load(Ordering::Relaxed) {
            // The buffer overflowed while nobody was listening: drop the stale output.
            tx.clear();
            OVERFLOW.store(false, Ordering::Relaxed);
        }
        if dtr && !was {
            SESSIONS.store(SESSIONS.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
        }
        if dtr {
            while tx.len > 0 {
                match usb.serial.write(tx.front()) {
                    Ok(n) if n > 0 => tx.consume(n),
                    _ => break,
                }
            }
            let _ = usb.serial.flush();
        }
    });
}

#[interrupt]
fn USBCTRL_IRQ() {
    service();
}

/// picotool reset interface for usb-device.
struct ResetClass {
    iface: InterfaceNumber,
}

impl<B: usb_device::bus::UsbBus> UsbClass<B> for ResetClass {
    fn get_configuration_descriptors(&self, writer: &mut DescriptorWriter) -> usb_device::Result<()> {
        writer.interface(self.iface, crate::RESET_CLASS, crate::RESET_SUBCLASS, crate::RESET_PROTOCOL)
    }

    fn control_out(&mut self, xfer: ControlOut<B>) {
        let req = xfer.request();
        if req.request_type == usb_device::control::RequestType::Class
            && req.recipient == usb_device::control::Recipient::Interface
            && req.index == u8::from(self.iface) as u16
        {
            crate::handle_reset_request(req.request, req.value);
            let _ = xfer.reject();
        }
    }
}
