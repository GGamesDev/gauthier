//! A tiny flash file system for the Raspberry Pi Pico (used by `picorust-runtime`).
//!
//! The area is split in 4 KiB sectors:
//!
//! * sectors 0 and 1 hold the directory, written alternately with a sequence
//!   number: an update writes the *other* copy, so a power cut in the middle of
//!   an update leaves the previous directory intact;
//! * the other sectors hold file data. A file uses contiguous sectors and is
//!   checked with a CRC-32. A file is replaced by writing the new data to free
//!   sectors first, then committing the directory.
//!
//! Directory sector (little-endian):
//!
//! | off | size | field                                  |
//! |-----|------|----------------------------------------|
//! | 0   | 4    | magic `PRFS`                           |
//! | 4   | 2    | version (1)                            |
//! | 6   | 2    | number of entries                      |
//! | 8   | 4    | sequence number                        |
//! | 12  | 4    | CRC-32 of the entries                  |
//! | 16  | 16   | reserved                               |
//! | 32  | 64×n | entries                                |
//!
//! Entry: name (48 bytes, UTF-8, zero padded), first sector (u16), sector count
//! (u16), length (u32), CRC-32 of the data (u32), reserved (u32).

#![cfg_attr(not(test), no_std)]

pub const SECTOR: usize = 4096;
pub const NAME_MAX: usize = 48;
pub const MAX_FILES: usize = (SECTOR - HEADER) / ENTRY;
const HEADER: usize = 32;
const ENTRY: usize = 64;
const MAGIC: &[u8; 4] = b"PRFS";
const VERSION: u16 = 1;
const DIR_SECTORS: usize = 2;

/// Raw access to the flash area (offsets are relative to the area).
pub trait Storage {
    /// Size of the area in bytes (a multiple of [`SECTOR`]).
    fn capacity(&self) -> usize;
    fn read(&mut self, offset: usize, buf: &mut [u8]);
    /// Erases one sector (sets it to 0xff).
    fn erase_sector(&mut self, offset: usize) -> Result<(), Error>;
    /// Programs bytes in an erased sector.
    fn program(&mut self, offset: usize, data: &[u8]) -> Result<(), Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NotFound,
    AlreadyExists,
    BadName,
    /// Not enough contiguous free space.
    NoSpace,
    TooManyFiles,
    /// The buffer given to `read` is too small.
    BufferTooSmall,
    /// The data written does not match the announced length or CRC.
    Mismatch,
    /// The flash did not accept the operation.
    Flash,
    /// A write is in progress (or was abandoned).
    Busy,
}

impl Error {
    pub fn as_str(self) -> &'static str {
        match self {
            Error::NotFound => "file not found",
            Error::AlreadyExists => "a file with this name already exists",
            Error::BadName => "invalid file name",
            Error::NoSpace => "not enough space on the Pico",
            Error::TooManyFiles => "too many files",
            Error::BufferTooSmall => "buffer too small",
            Error::Mismatch => "data does not match its checksum",
            Error::Flash => "flash write failed",
            Error::Busy => "busy",
        }
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xffff_ffff, data) ^ 0xffff_ffff
}

/// Incremental CRC-32 (start with 0xffff_ffff, finish with `^ 0xffff_ffff`).
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    crc
}

/// Names: 1..=48 bytes of UTF-8, no control characters, no `/`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_MAX
        && !name.chars().any(|c| c.is_control() || c == '/' || c == '\\')
        && name != "."
        && name != ".."
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    name: [u8; NAME_MAX],
    name_len: u8,
    pub first: u16,
    pub sectors: u16,
    pub len: u32,
    pub crc: u32,
}

impl Entry {
    const EMPTY: Entry = Entry { name: [0; NAME_MAX], name_len: 0, first: 0, sectors: 0, len: 0, crc: 0 };

    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
    }

    fn set_name(&mut self, name: &str) {
        self.name = [0; NAME_MAX];
        self.name[..name.len()].copy_from_slice(name.as_bytes());
        self.name_len = name.len() as u8;
    }
}

/// An open write: data goes to free sectors, the directory is updated by [`Fs::finish_write`].
#[derive(Debug)]
pub struct Writer {
    name: [u8; NAME_MAX],
    name_len: u8,
    first: u16,
    sectors: u16,
    len: u32,
    expected_crc: Option<u32>,
    written: u32,
    crc: u32,
    buf_len: usize,
}

pub struct Fs<S: Storage> {
    storage: S,
    entries: [Entry; MAX_FILES],
    count: usize,
    seq: u32,
    /// Directory sector holding the current directory (None: never written).
    active: Option<usize>,
    /// One sector of RAM for writes.
    buf: [u8; SECTOR],
    writing: bool,
}

impl<S: Storage> Fs<S> {
    /// Mounts the area (an erased or unknown area is an empty file system).
    pub fn mount(mut storage: S) -> Self {
        let mut best: Option<(usize, u32)> = None;
        let mut sector = [0u8; SECTOR];
        let mut entries = [Entry::EMPTY; MAX_FILES];
        let mut count = 0;
        for d in 0..DIR_SECTORS {
            storage.read(d * SECTOR, &mut sector);
            if let Some((seq, n)) = parse_header(&sector) {
                if best.map_or(true, |(_, s)| seq_newer(seq, s)) {
                    best = Some((d, seq));
                    count = n;
                    for (i, e) in entries.iter_mut().enumerate().take(n) {
                        *e = parse_entry(&sector[HEADER + i * ENTRY..HEADER + (i + 1) * ENTRY]);
                    }
                }
            }
        }
        Fs {
            storage,
            entries,
            count,
            seq: best.map_or(0, |(_, s)| s),
            active: best.map(|(d, _)| d),
            buf: [0; SECTOR],
            writing: false,
        }
    }

    pub fn storage(&mut self) -> &mut S {
        &mut self.storage
    }

    fn data_sectors(&self) -> usize {
        self.storage.capacity() / SECTOR - DIR_SECTORS
    }

    /// Bytes available for file data (total).
    pub fn capacity(&self) -> usize {
        self.data_sectors() * SECTOR
    }

    /// Free bytes (sum of free sectors; a file needs them contiguous).
    pub fn free(&self) -> usize {
        let used: usize = self.files().map(|e| e.sectors as usize).sum();
        (self.data_sectors() - used) * SECTOR
    }

    pub fn files(&self) -> impl Iterator<Item = &Entry> {
        self.entries[..self.count].iter()
    }

    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.files().find(|e| e.name() == name)
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.entries[..self.count].iter().position(|e| e.name() == name)
    }

    /// Reads a whole file into `buf`, returns its length. The CRC is checked.
    pub fn read(&mut self, name: &str, buf: &mut [u8]) -> Result<usize, Error> {
        let e = *self.find(name).ok_or(Error::NotFound)?;
        let len = e.len as usize;
        if buf.len() < len {
            return Err(Error::BufferTooSmall);
        }
        self.storage.read((DIR_SECTORS + e.first as usize) * SECTOR, &mut buf[..len]);
        if crc32(&buf[..len]) != e.crc {
            return Err(Error::Mismatch);
        }
        Ok(len)
    }

    /// Reads part of a file (no CRC check), returns the number of bytes read.
    pub fn read_at(&mut self, name: &str, offset: usize, buf: &mut [u8]) -> Result<usize, Error> {
        let e = *self.find(name).ok_or(Error::NotFound)?;
        let len = e.len as usize;
        if offset >= len {
            return Ok(0);
        }
        let n = buf.len().min(len - offset);
        self.storage.read((DIR_SECTORS + e.first as usize) * SECTOR + offset, &mut buf[..n]);
        Ok(n)
    }

    /// Writes a whole file (creates or replaces it).
    pub fn write(&mut self, name: &str, data: &[u8]) -> Result<(), Error> {
        let mut w = self.begin_write(name, data.len(), Some(crc32(data)))?;
        self.write_chunk(&mut w, data)?;
        self.finish_write(w)
    }

    /// Starts writing `len` bytes to `name`. Nothing changes until [`finish_write`](Self::finish_write).
    pub fn begin_write(&mut self, name: &str, len: usize, expected_crc: Option<u32>) -> Result<Writer, Error> {
        if !valid_name(name) {
            return Err(Error::BadName);
        }
        if self.writing {
            return Err(Error::Busy);
        }
        let replacing = self.index(name);
        if replacing.is_none() && self.count >= MAX_FILES {
            return Err(Error::TooManyFiles);
        }
        let need = len.div_ceil(SECTOR).max(1);
        // Always free sectors, even when replacing: the old copy must stay intact
        // until the directory is committed.
        let first = self.allocate(need).ok_or(Error::NoSpace)?;
        self.writing = true;
        let mut nm = [0u8; NAME_MAX];
        nm[..name.len()].copy_from_slice(name.as_bytes());
        Ok(Writer {
            name: nm,
            name_len: name.len() as u8,
            first: first as u16,
            sectors: need as u16,
            len: len as u32,
            expected_crc,
            written: 0,
            crc: 0xffff_ffff,
            buf_len: 0,
        })
    }

    pub fn write_chunk(&mut self, w: &mut Writer, mut data: &[u8]) -> Result<(), Error> {
        if w.written as usize + data.len() > w.len as usize {
            return Err(Error::Mismatch);
        }
        while !data.is_empty() {
            let n = (SECTOR - w.buf_len).min(data.len());
            self.buf[w.buf_len..w.buf_len + n].copy_from_slice(&data[..n]);
            w.crc = crc32_update(w.crc, &data[..n]);
            w.buf_len += n;
            w.written += n as u32;
            data = &data[n..];
            if w.buf_len == SECTOR {
                self.flush(w)?;
            }
        }
        Ok(())
    }

    fn flush(&mut self, w: &mut Writer) -> Result<(), Error> {
        let sector_index = (w.written as usize - 1) / SECTOR;
        let off = (DIR_SECTORS + w.first as usize + sector_index) * SECTOR;
        self.buf[w.buf_len..].fill(0xff);
        self.storage.erase_sector(off)?;
        self.storage.program(off, &self.buf)?;
        w.buf_len = 0;
        Ok(())
    }

    /// Ends a write: checks length and CRC, then commits the directory.
    pub fn finish_write(&mut self, mut w: Writer) -> Result<(), Error> {
        let result = (|| {
            if w.written != w.len {
                return Err(Error::Mismatch);
            }
            if w.buf_len > 0 || w.len == 0 {
                if w.len == 0 {
                    // Empty file: still own one erased sector.
                    let off = (DIR_SECTORS + w.first as usize) * SECTOR;
                    self.storage.erase_sector(off)?;
                } else {
                    self.flush(&mut w)?;
                }
            }
            let crc = w.crc ^ 0xffff_ffff;
            if w.expected_crc.is_some_and(|c| c != crc) {
                return Err(Error::Mismatch);
            }
            // Verify what the flash really holds.
            let mut check = 0xffff_ffffu32;
            let base = (DIR_SECTORS + w.first as usize) * SECTOR;
            let mut done = 0usize;
            while done < w.len as usize {
                let n = (w.len as usize - done).min(SECTOR);
                self.storage.read(base + done, &mut self.buf[..n]);
                check = crc32_update(check, &self.buf[..n]);
                done += n;
            }
            if check ^ 0xffff_ffff != crc {
                return Err(Error::Flash);
            }
            let name = core::str::from_utf8(&w.name[..w.name_len as usize]).map_err(|_| Error::BadName)?;
            let mut entry = Entry::EMPTY;
            entry.set_name(name);
            entry.first = w.first;
            entry.sectors = w.sectors;
            entry.len = w.len;
            entry.crc = crc;
            let mut entries = self.entries;
            let mut count = self.count;
            match self.index(name) {
                Some(i) => entries[i] = entry,
                None => {
                    entries[count] = entry;
                    count += 1;
                }
            }
            self.commit(entries, count)
        })();
        self.writing = false;
        result
    }

    /// Abandons a write (the data sectors simply stay free).
    pub fn abort_write(&mut self, _w: Writer) {
        self.writing = false;
    }

    pub fn remove(&mut self, name: &str) -> Result<(), Error> {
        let i = self.index(name).ok_or(Error::NotFound)?;
        let mut entries = self.entries;
        entries.copy_within(i + 1..self.count, i);
        self.commit(entries, self.count - 1)
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), Error> {
        if !valid_name(to) {
            return Err(Error::BadName);
        }
        let i = self.index(from).ok_or(Error::NotFound)?;
        if from == to {
            return Ok(());
        }
        if self.index(to).is_some() {
            return Err(Error::AlreadyExists);
        }
        let mut entries = self.entries;
        entries[i].set_name(to);
        self.commit(entries, self.count)
    }

    /// Deletes every file.
    pub fn format(&mut self) -> Result<(), Error> {
        self.commit([Entry::EMPTY; MAX_FILES], 0)
    }

    /// First run of `need` free data sectors.
    fn allocate(&self, need: usize) -> Option<usize> {
        let total = self.data_sectors();
        let mut start = 0;
        while start + need <= total {
            let clash = self.entries[..self.count]
                .iter()
                .map(|e| (e.first as usize, e.first as usize + e.sectors as usize))
                .filter(|&(a, b)| a < start + need && start < b)
                .map(|(_, b)| b)
                .max();
            match clash {
                None => return Some(start),
                Some(end) => start = end,
            }
        }
        None
    }

    fn commit(&mut self, entries: [Entry; MAX_FILES], count: usize) -> Result<(), Error> {
        let target = match self.active {
            Some(0) => 1,
            _ => 0,
        };
        let seq = self.seq.wrapping_add(1);
        let sector = &mut self.buf;
        sector.fill(0xff);
        for (i, e) in entries[..count].iter().enumerate() {
            write_entry(&mut sector[HEADER + i * ENTRY..HEADER + (i + 1) * ENTRY], e);
        }
        let crc = crc32(&sector[HEADER..HEADER + count * ENTRY]);
        sector[0..4].copy_from_slice(MAGIC);
        sector[4..6].copy_from_slice(&VERSION.to_le_bytes());
        sector[6..8].copy_from_slice(&(count as u16).to_le_bytes());
        sector[8..12].copy_from_slice(&seq.to_le_bytes());
        sector[12..16].copy_from_slice(&crc.to_le_bytes());
        sector[16..32].fill(0);
        let off = target * SECTOR;
        self.storage.erase_sector(off)?;
        self.storage.program(off, &self.buf)?;
        // Check it reads back as a valid directory before switching to it.
        self.storage.read(off, &mut self.buf);
        if parse_header(&self.buf) != Some((seq, count)) {
            return Err(Error::Flash);
        }
        self.entries = entries;
        self.count = count;
        self.seq = seq;
        self.active = Some(target);
        Ok(())
    }
}

/// `a` is newer than `b` (wrapping sequence numbers).
fn seq_newer(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000_0000
}

fn parse_header(s: &[u8]) -> Option<(u32, usize)> {
    if &s[0..4] != MAGIC || u16::from_le_bytes([s[4], s[5]]) != VERSION {
        return None;
    }
    let count = u16::from_le_bytes([s[6], s[7]]) as usize;
    if count > MAX_FILES {
        return None;
    }
    let seq = u32::from_le_bytes(s[8..12].try_into().unwrap());
    let crc = u32::from_le_bytes(s[12..16].try_into().unwrap());
    (crc32(&s[HEADER..HEADER + count * ENTRY]) == crc).then_some((seq, count))
}

fn parse_entry(b: &[u8]) -> Entry {
    let mut e = Entry::EMPTY;
    e.name.copy_from_slice(&b[..NAME_MAX]);
    e.name_len = b[..NAME_MAX].iter().position(|&c| c == 0).unwrap_or(NAME_MAX) as u8;
    e.first = u16::from_le_bytes([b[48], b[49]]);
    e.sectors = u16::from_le_bytes([b[50], b[51]]);
    e.len = u32::from_le_bytes(b[52..56].try_into().unwrap());
    e.crc = u32::from_le_bytes(b[56..60].try_into().unwrap());
    e
}

fn write_entry(b: &mut [u8], e: &Entry) {
    b[..NAME_MAX].copy_from_slice(&e.name);
    b[48..50].copy_from_slice(&e.first.to_le_bytes());
    b[50..52].copy_from_slice(&e.sectors.to_le_bytes());
    b[52..56].copy_from_slice(&e.len.to_le_bytes());
    b[56..60].copy_from_slice(&e.crc.to_le_bytes());
    b[60..64].fill(0);
}

#[cfg(test)]
mod tests;
