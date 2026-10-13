"""Reject PE imports that require Windows newer than 7 or a dynamic MSVC CRT.

This checks static and delay-load imports in the executable itself. It cannot
prove that Windows 7 has every transitive DLL dependency or that all runtime
paths work; an actual Windows 7 / Server 2012 R2 run remains necessary.
"""
from __future__ import annotations

import argparse
from pathlib import Path
import struct
import sys


class PEError(ValueError):
    pass


def number(data: bytes, offset: int, fmt: str) -> int:
    size = struct.calcsize(fmt)
    if offset < 0 or offset + size > len(data):
        raise PEError(f"truncated PE at file offset 0x{offset:x}")
    return struct.unpack_from(fmt, data, offset)[0]


def c_string(data: bytes, offset: int) -> str:
    if offset < 0 or offset >= len(data):
        raise PEError(f"invalid string offset 0x{offset:x}")
    end = data.find(b"\0", offset, min(offset + 4096, len(data)))
    if end < 0:
        raise PEError(f"unterminated string at file offset 0x{offset:x}")
    try:
        return data[offset:end].decode("ascii")
    except UnicodeDecodeError as exc:
        raise PEError(f"non-ASCII PE import at file offset 0x{offset:x}") from exc


def imports(data: bytes) -> tuple[tuple[int, int], list[tuple[str, str, str]]]:
    if data[:2] != b"MZ":
        raise PEError("missing MZ header")
    pe = number(data, 0x3C, "<I")
    if data[pe:pe + 4] != b"PE\0\0":
        raise PEError("missing PE signature")
    coff = pe + 4
    if number(data, coff, "<H") != 0x8664:
        raise PEError("expected an x86-64 PE image")
    count = number(data, coff + 2, "<H")
    optional_size = number(data, coff + 16, "<H")
    opt = coff + 20
    if opt + optional_size > len(data) or optional_size < 112:
        raise PEError("truncated PE32+ optional header")
    if number(data, opt, "<H") != 0x20B:
        raise PEError("expected a PE32+ image")
    min_os = (number(data, opt + 40, "<H"), number(data, opt + 42, "<H"))
    min_subsystem = (number(data, opt + 48, "<H"), number(data, opt + 50, "<H"))
    image_base = number(data, opt + 24, "<Q")
    size_of_headers = number(data, opt + 60, "<I")
    directory_count = number(data, opt + 108, "<I")
    section_start = opt + optional_size
    if section_start + count * 40 > len(data):
        raise PEError("truncated PE section table")
    sections = []
    for index in range(count):
        sec = section_start + index * 40
        sections.append((
            number(data, sec + 12, "<I"),  # VirtualAddress
            number(data, sec + 8, "<I"),   # VirtualSize
            number(data, sec + 16, "<I"),  # SizeOfRawData
            number(data, sec + 20, "<I"),  # PointerToRawData
        ))

    def at_rva(rva: int, size: int = 1) -> int:
        if rva < size_of_headers and rva + size <= min(size_of_headers, len(data)):
            return rva
        for start, virtual_size, raw_size, raw_start in sections:
            if start <= rva and rva + size <= start + max(virtual_size, raw_size):
                delta = rva - start
                if delta + size > raw_size or raw_start + delta + size > len(data):
                    break
                return raw_start + delta
        raise PEError(f"unmapped RVA 0x{rva:x}")

    def directory(index: int) -> tuple[int, int]:
        if index >= directory_count or 112 + (index + 1) * 8 > optional_size:
            return (0, 0)
        entry = opt + 112 + index * 8
        return (number(data, entry, "<I"), number(data, entry + 4, "<I"))

    found: list[tuple[str, str, str]] = []

    def thunk_names(rva: int, dll: str, kind: str) -> None:
        if not rva:
            raise PEError(f"missing lookup table for {dll}")
        for index in range(100000):
            value = number(data, at_rva(rva + index * 8, 8), "<Q")
            if value == 0:
                return
            if value & (1 << 63):
                name = f"#{value & 0xFFFF}"
            else:
                if value > 0xFFFFFFFF:
                    raise PEError(f"invalid import name RVA 0x{value:x}")
                name = c_string(data, at_rva(value, 2) + 2)
            found.append((kind, dll, name))
        raise PEError(f"unterminated import lookup table for {dll}")

    def walk(directory_index: int, descriptor_size: int, kind: str) -> None:
        rva, size = directory(directory_index)
        if rva == 0:
            return
        if size < descriptor_size:
            raise PEError(f"invalid {kind} directory size")
        for index in range(size // descriptor_size):
            off = at_rva(rva + index * descriptor_size, descriptor_size)
            desc = data[off:off + descriptor_size]
            if not any(desc):
                return
            if kind == "import":
                lookup = number(desc, 0, "<I") or number(desc, 16, "<I")
                name_rva = number(desc, 12, "<I")
            else:
                attrs = number(desc, 0, "<I")
                if attrs != 1:
                    raise PEError("delay import descriptor is not RVA-based")
                name_rva = number(desc, 4, "<I")
                lookup = number(desc, 16, "<I")
            dll = c_string(data, at_rva(name_rva))
            thunk_names(lookup, dll, kind)
        raise PEError(f"unterminated {kind} descriptor table")

    walk(1, 20, "import")
    walk(13, 32, "delay import")
    return (min_os, min_subsystem), found


UNSUPPORTED_SYMBOLS = {
    "setthreaddpiawarenesscontext",
    "setprocessdpiawarenesscontext",
    "getdpiforwindow",
    "getdpiforsystem",
    "getdpiformonitor",
    "waitonaddress",
    "wakebyaddresssingle",
    "wakebyaddressall",
    "getsystemtimepreciseasfiletime",
    "getsystemtimeadjustmentprecise",
    "processthreadprng",
    "processprng",
}
UNSUPPORTED_DLLS = {
    "bcryptprimitives.dll",
    "ucrtbase.dll",
    "ucrtbased.dll",
    "msvcrt.dll",
    "concrt140.dll",
}
CRT_PREFIXES = ("vcruntime", "msvcp", "msvcr", "api-ms-win-crt-", "ext-ms-win-crt-")


def audit(path: Path) -> int:
    versions, entries = imports(path.read_bytes())
    min_os, min_subsystem = versions
    dlls = sorted({dll.casefold() for _, dll, _ in entries})
    print(f"PE: {path}")
    print(f"Minimum OS: {min_os[0]}.{min_os[1]}; subsystem: {min_subsystem[0]}.{min_subsystem[1]}")
    print(f"Imports: {len(entries)} symbols from {len(dlls)} DLLs")
    print("DLLs: " + ", ".join(dlls))
    problems = []
    if not entries:
        problems.append("PE image has no import table; cannot audit Windows API dependencies")
    if min_os > (6, 1) or min_subsystem > (6, 1):
        problems.append("PE header requires a version newer than Windows 7 (6.1)")
    for kind, dll, symbol in entries:
        d = dll.casefold()
        s = symbol.casefold()
        if d in UNSUPPORTED_DLLS or d.startswith(CRT_PREFIXES):
            problems.append(f"{kind}: unsupported or dynamic CRT DLL {dll} ({symbol})")
        elif s in UNSUPPORTED_SYMBOLS:
            problems.append(f"{kind}: unsupported Windows API {dll}!{symbol}")
    if problems:
        for problem in problems:
            print(f"FAIL: {problem}", file=sys.stderr)
        return 1
    print("Windows 7 PE import audit passed.")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", type=Path, help="x86-64 Windows executable to audit")
    args = parser.parse_args()
    try:
        return audit(args.exe)
    except (OSError, PEError) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
