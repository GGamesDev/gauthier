//! File service on the second USB serial port (used by Gauthier's file explorer).
//!
//! Text protocol, fields separated by TAB, lines ended by `\n`:
//!
//! | request                         | answer                                             |
//! |---------------------------------|----------------------------------------------------|
//! | `HELLO`                         | `OK PRFS 1 <capacity> <free>`                      |
//! | `LIST`                          | `OK <n>` then n lines `<len> <crc hex> <name>`     |
//! | `GET <name>`                    | `OK <len> <crc hex>` then `len` raw bytes          |
//! | `PUT <name> <len> <crc hex>`    | `READY`, then the host sends `len` bytes, then `OK`|
//! | `DEL <name>`                    | `OK`                                               |
//! | `REN <old> <new>`               | `OK`                                               |
//! | `FORMAT`                        | `OK`                                               |
//!
//! Any failure answers `ERR <message>`.

use core::fmt::Write as _;

use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_usb::class::cdc_acm::CdcAcmClass;
use embassy_usb::driver::EndpointError;

use crate::fs;

type Class = CdcAcmClass<'static, Driver<'static, USB>>;

struct Link {
    class: Class,
    rx: [u8; 64],
    rx_len: usize,
    rx_pos: usize,
    tx: [u8; 64],
    tx_len: usize,
}

impl Link {
    async fn byte(&mut self) -> Result<u8, EndpointError> {
        while self.rx_pos == self.rx_len {
            self.rx_len = self.class.read_packet(&mut self.rx).await?;
            self.rx_pos = 0;
        }
        let b = self.rx[self.rx_pos];
        self.rx_pos += 1;
        Ok(b)
    }

    /// Reads one line (without `\n`, `\r` ignored). Over-long lines are truncated.
    async fn line<'a>(&mut self, buf: &'a mut [u8]) -> Result<&'a [u8], EndpointError> {
        let mut n = 0;
        loop {
            match self.byte().await? {
                b'\n' => return Ok(&buf[..n]),
                b'\r' => {}
                b => {
                    if n < buf.len() {
                        buf[n] = b;
                        n += 1;
                    }
                }
            }
        }
    }

    async fn read_into(&mut self, buf: &mut [u8]) -> Result<(), EndpointError> {
        let mut n = 0;
        while n < buf.len() {
            if self.rx_pos < self.rx_len {
                let k = (self.rx_len - self.rx_pos).min(buf.len() - n);
                buf[n..n + k].copy_from_slice(&self.rx[self.rx_pos..self.rx_pos + k]);
                self.rx_pos += k;
                n += k;
            } else {
                self.rx_len = self.class.read_packet(&mut self.rx).await?;
                self.rx_pos = 0;
            }
        }
        Ok(())
    }

    async fn send(&mut self, mut data: &[u8]) -> Result<(), EndpointError> {
        while !data.is_empty() {
            let k = (64 - self.tx_len).min(data.len());
            self.tx[self.tx_len..self.tx_len + k].copy_from_slice(&data[..k]);
            self.tx_len += k;
            data = &data[k..];
            if self.tx_len == 64 {
                self.class.write_packet(&self.tx).await?;
                self.tx_len = 0;
            }
        }
        Ok(())
    }

    /// Ends a message: sends the partial packet, or a zero-length packet after a full one.
    async fn flush(&mut self) -> Result<(), EndpointError> {
        self.class.write_packet(&self.tx[..self.tx_len]).await?;
        self.tx_len = 0;
        Ok(())
    }

    async fn send_line(&mut self, args: core::fmt::Arguments<'_>) -> Result<(), EndpointError> {
        let mut line = LineBuf::new();
        let _ = line.write_fmt(args);
        let _ = line.write_str("\n");
        self.send(line.as_bytes()).await
    }
}

struct LineBuf {
    buf: [u8; 160],
    len: usize,
}

impl LineBuf {
    fn new() -> Self {
        Self { buf: [0; 160], len: 0 }
    }
    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl core::fmt::Write for LineBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

fn parse_hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s, 16).ok()
}

macro_rules! reply {
    ($link:expr, $($arg:tt)*) => { $link.send_line(format_args!($($arg)*)).await? };
}

#[embassy_executor::task]
pub(crate) async fn files_task(class: Class) -> ! {
    let mut link = Link { class, rx: [0; 64], rx_len: 0, rx_pos: 0, tx: [0; 64], tx_len: 0 };
    loop {
        link.class.wait_connection().await;
        link.rx_len = 0;
        link.rx_pos = 0;
        link.tx_len = 0;
        let _ = serve(&mut link).await; // returns on disconnection
    }
}

async fn serve(link: &mut Link) -> Result<(), EndpointError> {
    let mut line_buf = [0u8; 200];
    loop {
        let raw = link.line(&mut line_buf).await?;
        let Ok(line) = core::str::from_utf8(raw) else {
            reply!(link, "ERR\tinvalid UTF-8");
            link.flush().await?;
            continue;
        };
        let mut parts = line.split('\t');
        let cmd = parts.next().unwrap_or("");
        let a = parts.next();
        let b = parts.next();
        let c = parts.next();

        match (cmd, a, b, c) {
            ("", None, None, None) => continue,
            ("HELLO", ..) => {
                let guard = fs::lock().await;
                let (cap, free) = guard.as_ref().map_or((0, 0), |f| (f.capacity(), f.free()));
                drop(guard);
                reply!(link, "OK\tPRFS\t1\t{}\t{}", cap, free);
            }
            ("LIST", ..) => {
                let guard = fs::lock().await;
                match guard.as_ref() {
                    Some(f) => {
                        reply!(link, "OK\t{}", f.files().count());
                        for e in f.files() {
                            reply!(link, "{}\t{:08x}\t{}", e.len, e.crc, e.name());
                        }
                    }
                    None => reply!(link, "ERR\tfile system not ready"),
                }
            }
            ("GET", Some(name), None, None) => {
                let mut guard = fs::lock().await;
                match guard.as_mut() {
                    Some(f) => match f.find(name).copied() {
                        Some(e) => {
                            reply!(link, "OK\t{}\t{:08x}", e.len, e.crc);
                            let mut chunk = [0u8; 256];
                            let mut off = 0usize;
                            while off < e.len as usize {
                                let n = f.read_at(name, off, &mut chunk).unwrap_or(0);
                                if n == 0 {
                                    break;
                                }
                                link.send(&chunk[..n]).await?;
                                off += n;
                            }
                        }
                        None => reply!(link, "ERR\t{}", fs::Error::NotFound.as_str()),
                    },
                    None => reply!(link, "ERR\tfile system not ready"),
                }
            }
            ("PUT", Some(name), Some(len), Some(crc)) => {
                let (Ok(len), Some(crc)) = (len.parse::<usize>(), parse_hex(crc)) else {
                    reply!(link, "ERR\tbad arguments");
                    link.flush().await?;
                    continue;
                };
                let mut guard = fs::lock().await;
                let Some(f) = guard.as_mut() else {
                    reply!(link, "ERR\tfile system not ready");
                    link.flush().await?;
                    continue;
                };
                match f.begin_write(name, len, Some(crc)) {
                    Err(e) => reply!(link, "ERR\t{}", e.as_str()),
                    Ok(mut w) => {
                        reply!(link, "READY");
                        link.flush().await?;
                        let mut chunk = [0u8; 256];
                        let mut left = len;
                        let mut failed = None;
                        while left > 0 {
                            let n = left.min(chunk.len());
                            if let Err(e) = link.read_into(&mut chunk[..n]).await {
                                f.abort_write(w);
                                return Err(e);
                            }
                            left -= n;
                            // After an error keep reading, so the stream stays in sync.
                            if failed.is_none() {
                                if let Err(e) = f.write_chunk(&mut w, &chunk[..n]) {
                                    failed = Some(e);
                                }
                            }
                        }
                        let result = match failed {
                            Some(e) => {
                                f.abort_write(w);
                                Err(e)
                            }
                            None => f.finish_write(w),
                        };
                        match result {
                            Ok(()) => reply!(link, "OK"),
                            Err(e) => reply!(link, "ERR\t{}", e.as_str()),
                        }
                    }
                }
            }
            ("DEL", Some(name), None, None) => {
                let r = match fs::lock().await.as_mut() {
                    Some(f) => f.remove(name),
                    None => Err(fs::Error::Busy),
                };
                match r {
                    Ok(()) => reply!(link, "OK"),
                    Err(e) => reply!(link, "ERR\t{}", e.as_str()),
                }
            }
            ("REN", Some(from), Some(to), None) => {
                let r = match fs::lock().await.as_mut() {
                    Some(f) => f.rename(from, to),
                    None => Err(fs::Error::Busy),
                };
                match r {
                    Ok(()) => reply!(link, "OK"),
                    Err(e) => reply!(link, "ERR\t{}", e.as_str()),
                }
            }
            ("FORMAT", None, None, None) => {
                let r = match fs::lock().await.as_mut() {
                    Some(f) => f.format(),
                    None => Err(fs::Error::Busy),
                };
                match r {
                    Ok(()) => reply!(link, "OK"),
                    Err(e) => reply!(link, "ERR\t{}", e.as_str()),
                }
            }
            ("DBG", ..) => {
                use core::sync::atomic::Ordering::Relaxed;
                use crate::embassy::*;
                reply!(
                    link,
                    "OK\tconnected={}\ttx_len={}\tstage={}\tpackets={}\tctrl_events={}",
                    is_connected_dbg(),
                    tx_len(),
                    DBG_TX_STAGE.load(Relaxed),
                    DBG_TX_PACKETS.load(Relaxed),
                    DBG_CTRL_EVENTS.load(Relaxed)
                );
            }
            _ => reply!(link, "ERR\tunknown command"),
        }
        link.flush().await?;
    }
}
