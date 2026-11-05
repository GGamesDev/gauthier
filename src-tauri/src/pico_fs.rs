//! Client of the picorust-runtime file service (second USB serial port of the Pico).
//! Works while the program runs: no BOOTSEL, no reboot.

use std::io::{Read, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::pico;

#[derive(Serialize, Clone, Debug)]
pub struct PicoFile {
    pub name: String,
    pub size: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct PicoFsInfo {
    pub capacity: u64,
    pub free: u64,
    pub files: Vec<PicoFile>,
}

struct Conn {
    path: String,
    port: Box<dyn serialport::SerialPort>,
    buf: Vec<u8>,
}

/// One connection, shared by every command (opened on demand, reopened after errors).
#[derive(Default)]
pub struct PicoFs {
    conn: Mutex<Option<Conn>>,
}

const TIMEOUT: Duration = Duration::from_secs(10);

impl Conn {
    fn open(path: &str) -> Result<Conn, String> {
        let mut port = serialport::new(path, 115_200)
            .timeout(Duration::from_millis(50))
            .open()
            .map_err(|e| format!("Cannot open the Pico's file port: {e}"))?;
        let _ = port.write_data_terminal_ready(true);
        let _ = port.clear(serialport::ClearBuffer::All);
        let mut c = Conn { path: path.to_string(), port, buf: Vec::new() };
        // Resynchronise: an empty line is ignored by the device.
        c.send(b"\n")?;
        Ok(c)
    }

    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.port.write_all(data).and_then(|_| self.port.flush()).map_err(|e| e.to_string())
    }

    fn fill(&mut self, deadline: Instant) -> Result<(), String> {
        let mut chunk = [0u8; 4096];
        loop {
            match self.port.read(&mut chunk) {
                Ok(n) if n > 0 => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    return Ok(());
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(format!("The Pico was disconnected ({e})")),
            }
            if Instant::now() > deadline {
                return Err("The Pico did not answer (is its program running picorust_runtime::init()?)".into());
            }
        }
    }

    fn line(&mut self, timeout: Duration) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=i).collect();
                return Ok(String::from_utf8_lossy(&line[..i]).trim_end_matches('\r').to_string());
            }
            self.fill(deadline)?;
        }
    }

    fn exact(&mut self, n: usize, timeout: Duration) -> Result<Vec<u8>, String> {
        let deadline = Instant::now() + timeout;
        while self.buf.len() < n {
            self.fill(deadline)?;
        }
        Ok(self.buf.drain(..n).collect())
    }

    fn command(&mut self, parts: &[&str]) -> Result<String, String> {
        for p in parts {
            if p.contains(['\t', '\n']) {
                return Err("Names cannot contain tabs or line breaks".into());
            }
        }
        self.send(format!("{}\n", parts.join("\t")).as_bytes())?;
        let reply = self.line(TIMEOUT)?;
        // Skip a stray empty answer left by the resync line.
        if reply.is_empty() { self.line(TIMEOUT) } else { Ok(reply) }
    }
}

fn check(reply: &str) -> Result<Vec<String>, String> {
    let mut parts = reply.split('\t').map(str::to_string);
    match parts.next().as_deref() {
        Some("OK") | Some("READY") => Ok(parts.collect()),
        Some("ERR") => {
            let msg = parts.collect::<Vec<_>>().join(" ");
            let mut c = msg.chars();
            Err(c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or(msg))
        }
        _ => Err(format!("Unexpected answer from the Pico: {reply}")),
    }
}

impl PicoFs {
    /// Runs `f` with a connection; reconnects once if the board was replugged/rebooted.
    fn with<T>(&self, mut f: impl FnMut(&mut Conn) -> Result<T, String>) -> Result<T, String> {
        let mut guard = self.conn.lock().unwrap();
        for attempt in 0..2 {
            let path = pico::files_port().ok_or("No Pico with PicoRust found on USB")?;
            if guard.as_ref().map(|c| c.path != path).unwrap_or(true) {
                *guard = Some(Conn::open(&path)?);
            }
            match f(guard.as_mut().unwrap()) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    *guard = None;
                    if attempt == 1 || !e.contains("disconnected") {
                        return Err(e);
                    }
                }
            }
        }
        unreachable!()
    }

    /// Forgets the connection (the board is being flashed).
    pub fn reset(&self) {
        *self.conn.lock().unwrap() = None;
    }

    pub fn info(&self) -> Result<PicoFsInfo, String> {
        self.with(|c| {
            let hello = check(&c.command(&["HELLO"])?)?;
            let capacity = hello.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            let free = hello.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let n: usize = check(&c.command(&["LIST"])?)?.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            let mut files = Vec::with_capacity(n);
            for _ in 0..n {
                let line = c.line(TIMEOUT)?;
                let mut p = line.splitn(3, '\t');
                let size = p.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let _crc = p.next();
                let name = p.next().unwrap_or("").to_string();
                files.push(PicoFile { name, size });
            }
            files.sort_by_key(|f| f.name.to_lowercase());
            Ok(PicoFsInfo { capacity, free, files })
        })
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        self.with(|c| {
            let head = check(&c.command(&["GET", name])?)?;
            let len: usize = head.first().and_then(|s| s.parse().ok()).ok_or("bad answer")?;
            let crc = head.get(1).and_then(|s| u32::from_str_radix(s, 16).ok()).ok_or("bad answer")?;
            let data = c.exact(len, TIMEOUT + Duration::from_millis(len as u64 / 20))?;
            if crc32(&data) != crc {
                return Err("The file was damaged during the transfer".into());
            }
            Ok(data)
        })
    }

    pub fn write(&self, name: &str, data: &[u8]) -> Result<(), String> {
        self.with(|c| {
            let crc = format!("{:08x}", crc32(data));
            check(&c.command(&["PUT", name, &data.len().to_string(), &crc])?)?;
            c.send(data)?;
            // Flash writes: ~50 ms per 4 KB sector.
            let wait = TIMEOUT + Duration::from_millis(data.len() as u64 / 40);
            check(&c.line(wait)?).map(|_| ())
        })
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        self.with(|c| check(&c.command(&["DEL", name])?).map(|_| ()))
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        self.with(|c| check(&c.command(&["REN", from, to])?).map(|_| ()))
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}
