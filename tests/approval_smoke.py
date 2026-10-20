"""Bounded loopback smoke for the real QR window and local access controls.

Run on an unlocked Windows desktop: python tests/approval_smoke.py --exe PATH
No request is approved through HTTP or a test-only host hook. Invitations and
connection files stay in a temporary directory and are never printed.
"""

import argparse
import ctypes
from ctypes import wintypes
import json
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import time
import zlib

from smoke import next_line, reader, stop


USER = ctypes.WinDLL("user32", use_last_error=True)
GDI = ctypes.WinDLL("gdi32", use_last_error=True)
WM_CLOSE = 0x0010
BM_CLICK = 0x00F5
SMTO_ABORTIFHUNG = 0x0002
ID_CONFIRM = 105
ID_APPROVE = 106
ID_DENY = 107
ID_STOP = 103
ENUMPROC = ctypes.WINFUNCTYPE(ctypes.c_int, wintypes.HWND, wintypes.LPARAM)

USER.EnumWindows.argtypes = [ENUMPROC, wintypes.LPARAM]
USER.EnumChildWindows.argtypes = [wintypes.HWND, ENUMPROC, wintypes.LPARAM]
USER.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
USER.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
USER.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
USER.GetDlgItem.argtypes = [wintypes.HWND, ctypes.c_int]
USER.GetDlgItem.restype = wintypes.HWND
USER.SetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPCWSTR]
USER.SetWindowTextW.restype = wintypes.BOOL
USER.IsWindowEnabled.argtypes = [wintypes.HWND]
USER.IsWindowEnabled.restype = wintypes.BOOL
USER.SendMessageTimeoutW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM,
                                      wintypes.LPARAM, wintypes.UINT, wintypes.UINT,
                                      ctypes.POINTER(ctypes.c_size_t)]
USER.SendMessageTimeoutW.restype = ctypes.c_size_t
USER.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
USER.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
USER.GetWindowDC.argtypes = [wintypes.HWND]
USER.GetWindowDC.restype = wintypes.HDC
USER.ReleaseDC.argtypes = [wintypes.HWND, wintypes.HDC]
USER.PrintWindow.argtypes = [wintypes.HWND, wintypes.HDC, wintypes.UINT]
USER.PrintWindow.restype = wintypes.BOOL
GDI.CreateCompatibleDC.argtypes = [wintypes.HDC]
GDI.CreateCompatibleDC.restype = wintypes.HDC
GDI.CreateCompatibleBitmap.argtypes = [wintypes.HDC, ctypes.c_int, ctypes.c_int]
GDI.CreateCompatibleBitmap.restype = wintypes.HBITMAP
GDI.SelectObject.argtypes = [wintypes.HDC, wintypes.HGDIOBJ]
GDI.SelectObject.restype = wintypes.HGDIOBJ
GDI.GetDIBits.argtypes = [wintypes.HDC, wintypes.HBITMAP, wintypes.UINT,
                         wintypes.UINT, ctypes.c_void_p, ctypes.c_void_p, wintypes.UINT]
GDI.DeleteObject.argtypes = [wintypes.HGDIOBJ]
GDI.DeleteDC.argtypes = [wintypes.HDC]


def wait_for(test, seconds, description):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = test()
        if value:
            return value
        time.sleep(0.08)
    raise AssertionError(description)


def choose_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def window_text(hwnd):
    buffer = ctypes.create_unicode_buffer(1024)
    USER.GetWindowTextW(hwnd, buffer, len(buffer))
    return buffer.value


def child_texts(hwnd):
    found = []

    @ENUMPROC
    def visit(child, unused):
        found.append(window_text(child))
        return 1

    USER.EnumChildWindows(hwnd, visit, 0)
    return found


def find_window(pid):
    found = []

    @ENUMPROC
    def visit(hwnd, unused):
        owner = wintypes.DWORD()
        USER.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value != pid:
            return 1
        name = ctypes.create_unicode_buffer(256)
        USER.GetClassNameW(hwnd, name, len(name))
        if name.value.startswith("WinremotePairingUI-" + str(pid) + "-"):
            found.append(hwnd)
        return 1

    USER.EnumWindows(visit, 0)
    assert len(found) <= 1, "multiple pairing windows in owned host"
    return found[0] if found else None



def find_save_dialog(pid):
    found=[]
    @ENUMPROC
    def visit(hwnd, unused):
        owner=wintypes.DWORD()
        USER.GetWindowThreadProcessId(hwnd,ctypes.byref(owner))
        name=ctypes.create_unicode_buffer(256)
        USER.GetClassNameW(hwnd,name,len(name))
        if owner.value==pid and name.value=="#32770" and "Salva il QR" in window_text(hwnd):
            found.append(hwnd)
        return 1
    USER.EnumWindows(visit,0)
    assert len(found)<=1, "multiple save dialogs in test host"
    return found[0] if found else None


def export_from_window(pid, hwnd, destination):
    assert USER.PostMessageW(hwnd, 0x0111, 102, 0)
    dialog=wait_for(lambda:find_save_dialog(pid),8,"save dialog did not appear")
    def filename_edit():
        edit=USER.GetDlgItem(dialog,1152)
        if edit:
            return edit
        candidates=[]
        @ENUMPROC
        def visit(child,unused):
            name=ctypes.create_unicode_buffer(256)
            USER.GetClassNameW(child,name,len(name))
            if name.value=="Edit":
                value=ctypes.create_unicode_buffer(1024)
                answer=ctypes.c_size_t()
                USER.SendMessageTimeoutW(child,0x000D,len(value),ctypes.addressof(value),
                    SMTO_ABORTIFHUNG,1000,ctypes.byref(answer))
                if value.value.startswith("winremote-invite"):
                    candidates.append(child)
            return 1
        USER.EnumChildWindows(dialog,visit,0)
        assert len(candidates)<=1, "ambiguous filename controls"
        return candidates[0] if candidates else None
    edit=wait_for(filename_edit,6,"filename edit control unavailable")
    value=ctypes.create_unicode_buffer(str(destination))
    answer=ctypes.c_size_t()
    assert USER.SendMessageTimeoutW(edit,0x00B1,0,-1,SMTO_ABORTIFHUNG,1000,ctypes.byref(answer))
    assert USER.SendMessageTimeoutW(edit,0x00C2,1,ctypes.addressof(value),SMTO_ABORTIFHUNG,1000,ctypes.byref(answer))
    click(USER.GetDlgItem(dialog,1))
    wait_for(destination.is_file,10,"private PNG export was not written")
    wait_for(lambda:not find_save_dialog(pid),5,"save dialog did not close")


def click(hwnd):
    answer = ctypes.c_size_t()
    assert USER.SendMessageTimeoutW(hwnd, BM_CLICK, 0, 0, SMTO_ABORTIFHUNG,
                                    2000, ctypes.byref(answer)), "UI control did not respond"


def chunk(kind, body):
    content = kind + body
    return struct.pack(">I", len(body)) + content + struct.pack(">I", zlib.crc32(content))


def capture_window_png(hwnd, path):
    """Use Win7 PrintWindow, then encode its GDI bitmap without external packages."""
    rect = wintypes.RECT()
    assert USER.GetWindowRect(hwnd, ctypes.byref(rect))
    width, height = rect.right - rect.left, rect.bottom - rect.top
    assert 200 <= width <= 2000 and 200 <= height <= 2000
    source = USER.GetWindowDC(hwnd)
    assert source
    memory = GDI.CreateCompatibleDC(source)
    bitmap = GDI.CreateCompatibleBitmap(source, width, height)
    old = None
    try:
        assert memory and bitmap
        old = GDI.SelectObject(memory, bitmap)
        assert old
        assert USER.PrintWindow(hwnd, memory, 0), "PrintWindow failed"
        GDI.SelectObject(memory, old)
        old = None
        info = ctypes.create_string_buffer(struct.pack("<IiiHHIIiiII", 40, width, -height,
                                1, 32, 0, width * height * 4, 0, 0, 0, 0))
        pixels = ctypes.create_string_buffer(width * height * 4)
        assert GDI.GetDIBits(source, bitmap, 0, height, pixels, info, 0) == height
        raw = memoryview(pixels).tobytes()
        rows = bytearray()
        for y in range(height):
            rows.append(0)
            start = y * width * 4
            for x in range(width):
                b, g, r, _ = raw[start + x * 4:start + x * 4 + 4]
                rows.extend((r, g, b))
        path.write_bytes(b"\x89PNG\r\n\x1a\n" +
                         chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)) +
                         chunk(b"IDAT", zlib.compress(rows, 6)) + chunk(b"IEND", b""))
    finally:
        if old:
            GDI.SelectObject(memory, old)
        if bitmap:
            GDI.DeleteObject(bitmap)
        if memory:
            GDI.DeleteDC(memory)
        USER.ReleaseDC(hwnd, source)


class Mcp:
    def __init__(self, exe, connection, diagnostic):
        self.process = subprocess.Popen([str(exe), "mcp", "--connection", str(connection)],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=diagnostic,
                text=True, encoding="utf-8")
        self.lines = reader(self.process.stdout)
        self.seq = 0
        self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                  "clientInfo": {"name": "approval-smoke", "version": "1"}})
        self.rpc("notifications/initialized", notify=True)

    def rpc(self, method, params=None, notify=False):
        self.seq += 1
        message = {"jsonrpc": "2.0", "method": method}
        if not notify:
            message["id"] = self.seq
        if params is not None:
            message["params"] = params
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()
        if notify:
            return None
        result = json.loads(next_line(self.lines, 30))
        assert result["id"] == self.seq and "error" not in result, "MCP RPC failed"
        return result["result"]

    def tool(self, name, arguments):
        result = self.rpc("tools/call", {"name": name, "arguments": arguments})
        assert not result.get("isError"), "MCP tool failed: " + name
        if "structuredContent" in result:
            return result["structuredContent"]
        return json.loads(result["content"][0]["text"])

    def close(self):
        stop(self.process)


def start_host(exe, ttl, work, diagnostic):
    port = choose_port()
    process = subprocess.Popen([str(exe), "host", "--qr", "--bind", "127.0.0.1",
        "--port", str(port), "--ttl-secs", str(ttl), "--require-admin", "false"],
        cwd=work, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=diagnostic,
        text=True, encoding="utf-8")
    try:
        invitation = next_line(reader(process.stdout), 20).strip()
        assert invitation.startswith("127.0.0.1:"), "unexpected invitation endpoint"
        hwnd = wait_for(lambda: find_window(process.pid), 10, "pairing UI did not appear")
        return process, port, hwnd
    except BaseException:
        stop(process)
        raise


def request(mcp, port):
    pending = mcp.tool("bridge_request_access", {"ip": "127.0.0.1", "port": port,
                                                   "client_label": "approval-smoke"})
    assert pending["status"] == "pending"
    return pending


def wait_status(mcp, request_id, wanted):
    def probe():
        result = mcp.tool("bridge_access_status", {"request_id": request_id, "wait_secs": 2})
        if result.get("status") == wanted or (wanted == "connected" and result.get("connected")):
            return result
        assert result.get("status") == "pending", "unexpected access result"
        return None
    return wait_for(probe, 25, "access decision did not reach MCP")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", required=True, type=Path)
    parser.add_argument("--allow-headless", action="store_true")
    args = parser.parse_args()
    exe = args.exe.resolve(strict=True)
    processes = []
    clients = []
    with tempfile.TemporaryDirectory(prefix="winremote-approval-smoke-") as directory:
        work = Path(directory)
        with (work / "diagnostic.txt").open("w", encoding="utf-8") as diagnostic:
            try:
                try:
                    host, port, hwnd = start_host(exe, 120, work, diagnostic)
                except AssertionError as error:
                    if args.allow_headless and "pairing UI did not appear" in str(error):
                        print("SKIP: interactive pairing UI unavailable", flush=True)
                        return
                    raise
                processes.append(host)
                image = work / "captured-window.png"
                capture_window_png(hwnd, image)
                qr_client = Mcp(exe, work / "qr-connection.json", diagnostic)
                clients.append(qr_client)
                assert qr_client.tool("bridge_connect_qr", {"image_path": str(image)})["connected"]
                print("PASS QR decoded from actual window capture", flush=True)
                exported=work / "exported-invitation.png"
                export_from_window(host.pid,hwnd,exported)
                assert qr_client.tool("bridge_connect_qr",{"image_path":str(exported)})["connected"]
                print("PASS Save PNG dialog, private export and QR connection",flush=True)


                client = Mcp(exe, work / "access-connection.json", diagnostic)
                clients.append(client)
                pending = request(client, port)
                code = pending["verification_code"]
                wait_for(lambda: any("Codice da confrontare: " + code in text
                                     for text in child_texts(hwnd)), 6,
                         "window comparison code differs from MCP")
                assert any("IP osservato: 127.0.0.1" in text for text in child_texts(hwnd))
                assert any("non verificato" in text for text in child_texts(hwnd))
                confirm = USER.GetDlgItem(hwnd, ID_CONFIRM)
                approve = USER.GetDlgItem(hwnd, ID_APPROVE)
                assert confirm and approve and not USER.IsWindowEnabled(approve)
                click(confirm)
                assert USER.IsWindowEnabled(approve), "Approve did not require local confirmation"
                click(approve)
                assert wait_status(client, pending["request_id"], "connected")["connected"]
                execution = client.tool("powershell_execute", {"script": "Write-Output 'APPROVAL_SMOKE_OK'",
                    "cwd": str(work), "timeout_secs": 10})
                assert "APPROVAL_SMOKE_OK" in json.dumps(execution)
                print("PASS local comparison, approval and authenticated command", flush=True)
                click(USER.GetDlgItem(hwnd, ID_STOP))
                host.wait(timeout=10)
                assert host.returncode == 0
                print("PASS Stop button stops host", flush=True)

                host2, port2, hwnd2 = start_host(exe, 90, work, diagnostic)
                processes.append(host2)
                client2 = Mcp(exe, work / "denied-connection.json", diagnostic)
                clients.append(client2)
                denied = request(client2, port2)
                wait_for(lambda: any("Codice da confrontare: " + denied["verification_code"] in text
                                     for text in child_texts(hwnd2)), 6, "deny request not shown")
                click(USER.GetDlgItem(hwnd2, ID_DENY))
                assert wait_status(client2, denied["request_id"], "denied")["connected"] is False
                assert not (work / "denied-connection.json").exists()
                print("PASS local deny leaves no connection file", flush=True)
                assert USER.PostMessageW(hwnd2, WM_CLOSE, 0, 0)
                host2.wait(timeout=10)
                assert host2.returncode == 0
                print("PASS window X stops host", flush=True)

                host3, _, hwnd3 = start_host(exe, 2, work, diagnostic)
                processes.append(host3)
                host3.wait(timeout=10)
                assert not find_window(host3.pid)
                print("PASS TTL closes window", flush=True)
                host4, _, hwnd4 = start_host(exe, 4, work, diagnostic)
                processes.append(host4)
                assert USER.PostMessageW(hwnd4,0x0111,102,0)
                wait_for(lambda:find_save_dialog(host4.pid),2,"save dialog did not open before TTL")
                host4.wait(timeout=10)
                assert host4.returncode==0 and not find_window(host4.pid)
                print("PASS TTL closes host during modal Save PNG dialog",flush=True)

            finally:
                for client in reversed(clients):
                    client.close()
                for process in reversed(processes):
                    stop(process)
    print("All local approval smoke checks passed; temporary data removed.", flush=True)


if __name__ == "__main__":
    main()
