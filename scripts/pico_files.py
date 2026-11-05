#!/usr/bin/env python3
"""Reads the files stored on a Raspberry Pi Pico by picorust-runtime, without Gauthier.

Independent check of the on-flash format (picorust-runtime/fs-core): talks to the
board with picotool only, decodes both directory copies, picks the newest valid
one and verifies every file's CRC-32.

    scripts/pico_files.py list                    # files and their state
    scripts/pico_files.py verify main.rs file.rs  # exit 0 only if identical, byte for byte
    scripts/pico_files.py dump main.rs out.rs     # copy a file to the computer

(The board is rebooted into BOOTSEL by picotool and back to its program: this tool
is for testing; Gauthier itself reads the files over USB without rebooting.)
"""

import shutil
import struct
import subprocess
import sys
import tempfile
import time
import zlib
from pathlib import Path

SECTOR = 4096
AREA = 512 * 1024
AREA_START = {"pico": 0x10000000 + 2048 * 1024 - AREA, "pico2": 0x10000000 + 4096 * 1024 - AREA}
HEADER, ENTRY = 32, 64


def picotool(*args, check=True):
    exe = shutil.which("picotool") or str(
        Path.home() / "Library/Application Support/Gauthier/tools/picotool-2.3.1/picotool/picotool"
    )
    p = subprocess.run([exe, *args], capture_output=True, text=True)
    if check and p.returncode != 0:
        sys.exit(f"picotool {' '.join(args)} failed:\n{p.stdout}{p.stderr}")
    return p


def in_bootsel():
    p = picotool("info", check=False)
    return p.returncode == 0 and "No accessible" not in p.stdout + p.stderr


def read_area():
    if not in_bootsel():
        picotool("reboot", "-u", "-f", check=False)
        for _ in range(30):
            time.sleep(0.3)
            if in_bootsel():
                break
        else:
            sys.exit("The Pico did not go into BOOTSEL mode.")
    try:
        board = "pico2" if "RP235" in picotool("info", "-d").stdout else "pico"
        start = AREA_START[board]
        with tempfile.TemporaryDirectory() as d:
            out = Path(d) / "area.bin"
            picotool("save", "-r", f"{start:08x}", f"{start + AREA:08x}", str(out), "-t", "bin")
            return board, out.read_bytes()
    finally:
        picotool("reboot", check=False)


def parse_dir(sector):
    if sector[:4] != b"PRFS":
        return None
    version, count, seq, crc = struct.unpack_from("<HHII", sector, 4)
    if version != 1 or count > (SECTOR - HEADER) // ENTRY:
        return None
    if zlib.crc32(sector[HEADER : HEADER + count * ENTRY]) != crc:
        return None
    files = []
    for i in range(count):
        e = sector[HEADER + i * ENTRY : HEADER + (i + 1) * ENTRY]
        name = e[:48].split(b"\0", 1)[0].decode("utf-8", "replace")
        first, sectors, length, fcrc = struct.unpack_from("<HHII", e, 48)
        files.append({"name": name, "first": first, "sectors": sectors, "len": length, "crc": fcrc})
    return seq, files


def newest(a, b):
    if a is None:
        return b
    if b is None:
        return a
    return a if ((a[0] - b[0]) & 0xFFFFFFFF) < 0x80000000 else b


def main():
    if len(sys.argv) < 2 or sys.argv[1] not in ("list", "verify", "dump"):
        sys.exit(__doc__)
    board, area = read_area()
    d0, d1 = parse_dir(area[:SECTOR]), parse_dir(area[SECTOR : 2 * SECTOR])
    best = newest(d0, d1)
    print(f"board {board}: directory copies valid: {d0 is not None}/{d1 is not None}")
    if best is None:
        sys.exit("no file system on this Pico (never written)")
    seq, files = best
    print(f"directory sequence {seq}, {len(files)} file(s)")
    data = {}
    bad = 0
    for f in files:
        off = (2 + f["first"]) * SECTOR
        content = area[off : off + f["len"]]
        ok = zlib.crc32(content) == f["crc"]
        bad += not ok
        data[f["name"]] = content
        print(f"  {'OK ' if ok else 'BAD'} {f['len']:>8} B  sectors {f['first']}+{f['sectors']}  {f['name']}")
    if bad:
        sys.exit(f"{bad} file(s) with a wrong CRC")

    if sys.argv[1] == "verify":
        name, path = sys.argv[2], sys.argv[3]
        expected = Path(path).read_bytes()
        if name not in data:
            sys.exit(f"MISSING: {name} is not on the Pico")
        if data[name] != expected:
            sys.exit(f"MISMATCH: {name} differs from {path} ({len(data[name])} vs {len(expected)} bytes)")
        print(f"OK: {name} on the Pico is identical to {path} ({len(expected)} bytes)")
    elif sys.argv[1] == "dump":
        Path(sys.argv[3]).write_bytes(data[sys.argv[2]])
        print(f"saved {sys.argv[2]} to {sys.argv[3]}")


if __name__ == "__main__":
    main()
