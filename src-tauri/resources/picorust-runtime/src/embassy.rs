//! Embassy (embassy-rp + embassy-usb) implementation.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_time::Timer;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler};
use embassy_rp::{bind_interrupts, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pipe::Pipe;
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, Sender, State};
use embassy_usb::control::{OutResponse, Recipient, Request, RequestType};
#[cfg(feature = "winusb")]
use embassy_usb::msos::{self, windows_version};
use embassy_usb::types::InterfaceNumber;
use embassy_usb::{Builder, Handler, UsbDevice};
use static_cell::StaticCell;

use crate::safe_mode;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

type UsbDriver = Driver<'static, USB>;

const TX_CAP: usize = 2048;
const RX_CAP: usize = 512;

static TX: Pipe<CriticalSectionRawMutex, TX_CAP> = Pipe::new();
static RX: Pipe<CriticalSectionRawMutex, RX_CAP> = Pipe::new();
static CONNECTED: AtomicBool = AtomicBool::new(false);
/// Diagnostics (read with the `DBG` command of the file service).
pub(crate) static DBG_TX_STAGE: AtomicU32 = AtomicU32::new(0);
pub(crate) static DBG_TX_PACKETS: AtomicU32 = AtomicU32::new(0);
pub(crate) static DBG_CTRL_EVENTS: AtomicU32 = AtomicU32::new(0);
/// Set when output had to be dropped because the buffer was full.
static OVERFLOW: AtomicBool = AtomicBool::new(false);
/// Incremented each time a terminal opens the port (single writer: the USB control path).
static SESSIONS: AtomicU32 = AtomicU32::new(0);

/// Starts the USB console, the file service and the reset interface.
///
/// picorust-runtime uses the `FLASH` peripheral for its files (see [`crate::fs`]):
/// do not use it elsewhere.
///
/// Call it at the very beginning of `main`, before spawning your own tasks:
///
/// ```ignore
/// let p = embassy_rp::init(Default::default());
/// picorust_runtime::init(spawner, p.USB).await;
/// ```
///
/// If the previous run panicked, this future never completes (safe mode).
pub async fn init(spawner: Spawner, usb: Peri<'static, USB>) {
    let safe = safe_mode::take_at_boot();
    crate::fs::mount();

    let driver = Driver::new(usb, Irqs);

    let mut config = embassy_usb::Config::new(crate::USB_VID, crate::USB_PID);
    config.manufacturer = Some("PicoRust");
    config.product = Some(crate::USB_PRODUCT);
    // Same descriptors in safe mode: macOS asks to approve every "new" accessory.
    config.serial_number = Some("picorust");
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static MSOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    static CDC_STATE: StaticCell<State> = StaticCell::new();
    static FILES_STATE: StaticCell<State> = StaticCell::new();
    static RESET_HANDLER: StaticCell<ResetHandler> = StaticCell::new();

    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        MSOS_DESC.init([0; 256]),
        CONTROL_BUF.init([0; 64]),
    );
    // MS OS 2.0 descriptors let Windows bind WinUSB to the reset interface
    // automatically, so picotool works there without Zadig.
    #[cfg(feature = "winusb")]
    builder.msos_descriptor(windows_version::WIN8_1, 0x01);

    // First serial port: the console. Second one: the file service (Gauthier's explorer).
    let class = CdcAcmClass::new(&mut builder, CDC_STATE.init(State::new()), 64);
    let files = CdcAcmClass::new(&mut builder, FILES_STATE.init(State::new()), 64);

    let iface_num;
    {
        let mut func = builder.function(crate::RESET_CLASS, crate::RESET_SUBCLASS, crate::RESET_PROTOCOL);
        #[cfg(feature = "winusb")]
        {
            func.msos_feature(msos::CompatibleIdFeatureDescriptor::new("WINUSB", ""));
            func.msos_feature(msos::RegistryPropertyFeatureDescriptor::new(
                "DeviceInterfaceGUIDs",
                msos::PropertyData::RegMultiSz(&["{ecceff35-146c-4ff3-acd9-8f992d09acdd}"]),
            ));
        }
        let mut iface = func.interface();
        iface_num = iface.interface_number();
        let _alt = iface.alt_setting(crate::RESET_CLASS, crate::RESET_SUBCLASS, crate::RESET_PROTOCOL, None);
    }
    builder.handler(RESET_HANDLER.init(ResetHandler { iface: iface_num }));

    let usb = builder.build();
    let (tx, rx, ctrl) = class.split_with_control();

    spawner.spawn(usb_task(usb).unwrap());
    spawner.spawn(tx_task(tx).unwrap());
    spawner.spawn(rx_task(rx, ctrl).unwrap());
    spawner.spawn(crate::files_service::files_task(files).unwrap());

    if safe {
        // Safe mode: the user firmware does not run. Show the panic message to
        // every terminal that connects (after a short delay: the host may flush
        // its input buffer right after opening the port).
        let mut shown = 0;
        loop {
            let session = SESSIONS.load(Ordering::Relaxed);
            if session != shown && is_connected() {
                Timer::after_millis(300).await;
                safe_mode::write_banner(write);
                shown = session;
            }
            Timer::after_millis(50).await;
        }
    }
}

/// True when a terminal has the serial port open (DTR set).
pub(crate) fn tx_len() -> usize {
    TX.len()
}

/// True when a terminal has the serial port open (DTR set).
pub fn is_connected_dbg() -> bool {
    CONNECTED.load(Ordering::Relaxed)
}

pub fn is_connected() -> bool {
    CONNECTED.load(Ordering::Relaxed)
}

/// Queues bytes for the USB console. Never blocks. Output written before a
/// terminal connects is kept (up to the buffer size) and shown when it connects.
pub fn write(bytes: &[u8]) {
    let mut rest = bytes;
    while !rest.is_empty() {
        match TX.try_write(rest) {
            Ok(n) => rest = &rest[n..],
            Err(_) => {
                OVERFLOW.store(true, Ordering::Relaxed);
                break;
            }
        }
    }
}

/// Writes bytes to the USB console, waiting for buffer space.
pub async fn write_async(bytes: &[u8]) {
    if is_connected() {
        TX.write_all(bytes).await;
    }
}

/// Reads bytes typed in the VS Code terminal. Waits until at least one byte is available.
pub async fn read(buf: &mut [u8]) -> usize {
    RX.read(buf).await
}

/// Reads one line typed in the VS Code terminal (without the line ending).
/// Returns the number of bytes stored in `buf`.
pub async fn read_line(buf: &mut [u8]) -> usize {
    let mut len = 0;
    loop {
        let mut b = [0u8; 1];
        RX.read(&mut b).await;
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

/// Non-blocking read; returns 0 when nothing is available.
pub fn try_read(buf: &mut [u8]) -> usize {
    RX.try_read(buf).unwrap_or(0)
}

#[embassy_executor::task]
async fn usb_task(mut usb: UsbDevice<'static, UsbDriver>) -> ! {
    usb.run().await
}

#[embassy_executor::task]
async fn tx_task(mut tx: Sender<'static, UsbDriver>) -> ! {
    let mut buf = [0u8; 64];
    loop {
        DBG_TX_STAGE.store(1, Ordering::Relaxed);
        tx.wait_connection().await;
        DBG_TX_STAGE.store(2, Ordering::Relaxed);
        // Keep the output queued until a terminal opens the port: data sent
        // earlier would be thrown away by the host when the port is opened.
        while !is_connected() {
            Timer::after_millis(10).await;
        }
        Timer::after_millis(50).await;
        DBG_TX_STAGE.store(3, Ordering::Relaxed);
        loop {
            let disconnected = async {
                while is_connected() {
                    Timer::after_millis(20).await;
                }
            };
            let n = match select(TX.read(&mut buf), disconnected).await {
                Either::First(n) => n,
                Either::Second(()) => break,
            };
            DBG_TX_STAGE.store(4, Ordering::Relaxed);
            if tx.write_packet(&buf[..n]).await.is_err() {
                break;
            }
            DBG_TX_STAGE.store(3, Ordering::Relaxed);
            DBG_TX_PACKETS.store(DBG_TX_PACKETS.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
            // A full packet must be followed by a zero length packet when it
            // ends the transfer, otherwise the host keeps waiting.
            if n == buf.len() && TX.is_empty() && tx.write_packet(&[]).await.is_err() {
                break;
            }
        }
    }
}

/// Receives terminal input and watches the control lines (DTR, line coding).
#[embassy_executor::task]
async fn rx_task(mut rx: Receiver<'static, UsbDriver>, ctrl: ControlChanged<'static>) -> ! {
    let mut buf = [0u8; 64];
    loop {
        match select(rx.read_packet(&mut buf), ctrl.control_changed()).await {
            Either::First(Ok(n)) => {
                let _ = RX.try_write(&buf[..n]);
            }
            Either::First(Err(_)) => {
                CONNECTED.store(false, Ordering::Relaxed);
                rx.wait_connection().await;
            }
            Either::Second(()) => on_control_changed(rx.line_coding().data_rate(), ctrl.dtr()),
        }
    }
}

fn on_control_changed(baud: u32, dtr: bool) {
    DBG_CTRL_EVENTS.store(DBG_CTRL_EVENTS.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
    if baud == crate::MAGIC_BAUD {
        crate::reboot::reboot_to_bootsel();
    }
    // load + store (no CAS on Cortex-M0+); this is the only writer.
    let was = CONNECTED.load(Ordering::Relaxed);
    CONNECTED.store(dtr, Ordering::Relaxed);
    if dtr && !was {
        // Fresh terminal: keep what was printed meanwhile, unless the buffer
        // overflowed (then it only holds old, truncated output).
        if OVERFLOW.load(Ordering::Relaxed) {
            TX.clear();
            OVERFLOW.store(false, Ordering::Relaxed);
        }
        SESSIONS.store(SESSIONS.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
    }
}

struct ResetHandler {
    iface: InterfaceNumber,
}

impl Handler for ResetHandler {
    fn control_out(&mut self, req: Request, _data: &[u8]) -> Option<OutResponse> {
        if req.request_type != RequestType::Class
            || req.recipient != Recipient::Interface
            || req.index != self.iface.0 as u16
        {
            return None;
        }
        crate::handle_reset_request(req.request, req.value);
        Some(OutResponse::Rejected)
    }
}
