use super::*;

/// RAM flash that behaves like NOR flash: programming can only clear bits,
/// and it can simulate a power cut after N operations.
struct RamFlash {
    data: Vec<u8>,
    ops: usize,
    fail_after: Option<usize>,
    erases: usize,
}

impl RamFlash {
    fn new(size: usize) -> Self {
        RamFlash { data: vec![0xff; size], ops: 0, fail_after: None, erases: 0 }
    }
    fn tick(&mut self) -> Result<(), Error> {
        self.ops += 1;
        match self.fail_after {
            Some(n) if self.ops > n => Err(Error::Flash),
            _ => Ok(()),
        }
    }
}

impl Storage for RamFlash {
    fn capacity(&self) -> usize {
        self.data.len()
    }
    fn read(&mut self, offset: usize, buf: &mut [u8]) {
        buf.copy_from_slice(&self.data[offset..offset + buf.len()]);
    }
    fn erase_sector(&mut self, offset: usize) -> Result<(), Error> {
        self.tick()?;
        assert_eq!(offset % SECTOR, 0);
        self.erases += 1;
        self.data[offset..offset + SECTOR].fill(0xff);
        Ok(())
    }
    fn program(&mut self, offset: usize, data: &[u8]) -> Result<(), Error> {
        self.tick()?;
        for (d, s) in self.data[offset..offset + data.len()].iter_mut().zip(data) {
            *d &= *s; // NOR: only 1 -> 0
        }
        Ok(())
    }
}

const SIZE: usize = 512 * 1024;

fn fs() -> Fs<RamFlash> {
    Fs::mount(RamFlash::new(SIZE))
}

fn remount(fs: Fs<RamFlash>) -> Fs<RamFlash> {
    let Fs { storage, .. } = fs;
    Fs::mount(RamFlash { fail_after: None, ops: 0, ..storage })
}

fn read_all(fs: &mut Fs<RamFlash>, name: &str) -> Vec<u8> {
    let mut buf = vec![0u8; 600 * 1024];
    let n = fs.read(name, &mut buf).unwrap();
    buf.truncate(n);
    buf
}

#[test]
fn empty_flash_is_an_empty_fs() {
    let f = fs();
    assert_eq!(f.files().count(), 0);
    assert_eq!(f.capacity(), SIZE - 2 * SECTOR);
    assert_eq!(f.free(), f.capacity());
}

#[test]
fn write_read_list() {
    let mut f = fs();
    f.write("main.rs", b"fn main() {}\n").unwrap();
    f.write("notes.txt", "Héllo 😀".as_bytes()).unwrap();
    assert_eq!(read_all(&mut f, "main.rs"), b"fn main() {}\n");
    assert_eq!(read_all(&mut f, "notes.txt"), "Héllo 😀".as_bytes());
    let names: Vec<_> = f.files().map(|e| e.name().to_string()).collect();
    assert_eq!(names, ["main.rs", "notes.txt"]);
}

#[test]
fn survives_remount() {
    let mut f = fs();
    f.write("a.txt", b"alpha").unwrap();
    f.write("b.txt", b"beta").unwrap();
    f.remove("a.txt").unwrap();
    let mut f = remount(f);
    assert!(f.find("a.txt").is_none());
    assert_eq!(read_all(&mut f, "b.txt"), b"beta");
}

#[test]
fn replace_keeps_one_copy_and_frees_space() {
    let mut f = fs();
    let free0 = f.free();
    f.write("x", &vec![1u8; 10_000]).unwrap(); // 3 sectors
    assert_eq!(f.free(), free0 - 3 * SECTOR);
    f.write("x", b"short").unwrap(); // 1 sector
    assert_eq!(f.files().count(), 1);
    assert_eq!(f.free(), free0 - SECTOR);
    assert_eq!(read_all(&mut f, "x"), b"short");
}

#[test]
fn sizes_at_sector_boundaries() {
    let mut f = fs();
    for len in [0usize, 1, 4095, 4096, 4097, 8192, 100_000] {
        let data: Vec<u8> = (0..len).map(|i| (i * 7 + len) as u8).collect();
        f.write("f.bin", &data).unwrap();
        assert_eq!(read_all(&mut f, "f.bin"), data, "len {len}");
        let f2 = remount(f);
        f = f2;
        assert_eq!(read_all(&mut f, "f.bin"), data, "len {len} after remount");
    }
}

#[test]
fn rename_and_errors() {
    let mut f = fs();
    f.write("a", b"1").unwrap();
    f.write("b", b"2").unwrap();
    assert_eq!(f.rename("a", "b"), Err(Error::AlreadyExists));
    assert_eq!(f.rename("zz", "c"), Err(Error::NotFound));
    assert_eq!(f.rename("a", "bad/name"), Err(Error::BadName));
    f.rename("a", "c").unwrap();
    assert_eq!(read_all(&mut f, "c"), b"1");
    assert!(f.find("a").is_none());
    assert_eq!(f.remove("a"), Err(Error::NotFound));
    let mut small = [0u8; 0];
    assert_eq!(f.read("c", &mut small), Err(Error::BufferTooSmall));
}

#[test]
fn names() {
    let mut f = fs();
    assert_eq!(f.write("", b""), Err(Error::BadName));
    assert_eq!(f.write(&"a".repeat(49), b""), Err(Error::BadName));
    assert_eq!(f.write("tab\there", b""), Err(Error::BadName));
    assert_eq!(f.write("..", b""), Err(Error::BadName));
    f.write(&"é".repeat(24), b"ok").unwrap(); // 48 bytes
    f.write("with space.rs", b"ok").unwrap();
    let mut f = remount(f);
    assert_eq!(read_all(&mut f, &"é".repeat(24)), b"ok");
}

#[test]
fn full_disk_and_too_many_files() {
    let mut f = fs();
    let big = vec![0xabu8; f.capacity()];
    f.write("big", &big).unwrap();
    assert_eq!(f.free(), 0);
    assert_eq!(f.write("one-more", b"x"), Err(Error::NoSpace));
    // Replacing a file that fills the disk is not possible (no room for the new copy)…
    assert_eq!(f.write("big", b"small"), Err(Error::NoSpace));
    // …the old data is untouched.
    assert_eq!(read_all(&mut f, "big"), big);
    f.remove("big").unwrap();
    for i in 0..MAX_FILES {
        f.write(&format!("f{i}"), b"x").unwrap();
    }
    assert_eq!(f.write("extra", b"x"), Err(Error::TooManyFiles));
    f.write("f0", b"replace is fine").unwrap();
}

#[test]
fn fragmentation_uses_holes() {
    let mut f = fs();
    f.write("a", &vec![1; SECTOR]).unwrap();
    f.write("b", &vec![2; SECTOR]).unwrap();
    f.write("c", &vec![3; SECTOR]).unwrap();
    f.remove("b").unwrap();
    f.write("d", b"fits in the hole").unwrap();
    assert_eq!(f.find("d").unwrap().first, 1);
    assert_eq!(read_all(&mut f, "c"), vec![3; SECTOR]);
}

#[test]
fn checksum_mismatch_is_refused_and_changes_nothing() {
    let mut f = fs();
    f.write("keep", b"old").unwrap();
    let mut w = f.begin_write("keep", 3, Some(0xdead_beef)).unwrap();
    f.write_chunk(&mut w, b"new").unwrap();
    assert_eq!(f.finish_write(w), Err(Error::Mismatch));
    assert_eq!(read_all(&mut f, "keep"), b"old");
    // Too much / too little data.
    let mut w = f.begin_write("keep", 2, None).unwrap();
    assert_eq!(f.write_chunk(&mut w, b"abc"), Err(Error::Mismatch));
    f.abort_write(w);
    let mut w = f.begin_write("keep", 5, None).unwrap();
    f.write_chunk(&mut w, b"ab").unwrap();
    assert_eq!(f.finish_write(w), Err(Error::Mismatch));
    assert_eq!(read_all(&mut f, "keep"), b"old");
}

#[test]
fn streaming_write_in_odd_chunks() {
    let mut f = fs();
    let data: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let mut w = f.begin_write("s", data.len(), Some(crc32(&data))).unwrap();
    for chunk in data.chunks(63) {
        f.write_chunk(&mut w, chunk).unwrap();
    }
    f.finish_write(w).unwrap();
    assert_eq!(read_all(&mut f, "s"), data);
    let mut part = [0u8; 10];
    assert_eq!(f.read_at("s", 49_995, &mut part).unwrap(), 5);
    assert_eq!(&part[..5], &data[49_995..]);
}

/// Power cut at every possible point of an update: after remount the file
/// system is either the old state or the new state, never broken.
#[test]
fn power_cut_at_any_point() {
    let mut base = fs();
    base.write("main.rs", b"version 1").unwrap();
    base.write("other", b"untouched").unwrap();
    let Fs { storage, .. } = base;
    let snapshot = storage.data.clone();

    for cut in 0..40 {
        let flash = RamFlash { data: snapshot.clone(), ops: 0, fail_after: Some(cut), erases: 0 };
        let mut f = Fs::mount(flash);
        let result = f.write("main.rs", &vec![b'2'; 9000]);
        let mut f = remount(f);
        let got = read_all(&mut f, "main.rs");
        assert!(got == b"version 1" || got == vec![b'2'; 9000], "cut {cut}: broken state");
        assert_eq!(read_all(&mut f, "other"), b"untouched", "cut {cut}");
        if result.is_ok() {
            assert_eq!(got, vec![b'2'; 9000]);
        }
    }
}

#[test]
fn many_updates_alternate_directory_copies() {
    let mut f = fs();
    for i in 0..300u32 {
        f.write("counter", &i.to_le_bytes()).unwrap();
    }
    let mut f = remount(f);
    assert_eq!(read_all(&mut f, "counter"), 299u32.to_le_bytes());
    assert_eq!(f.files().count(), 1);
}

#[test]
fn sequence_wraps_around() {
    assert!(seq_newer(1, 0));
    assert!(seq_newer(0, u32::MAX));
    assert!(!seq_newer(u32::MAX, 0));
}

#[test]
fn crc_reference() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
}
