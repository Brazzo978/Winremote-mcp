"""Local end-to-end test. Uses only loopback and temporary files; never logs invitations."""
import argparse
import base64
import ctypes
import json
import hmac
import hashlib
import struct
import os
from pathlib import Path
import queue
import socket
import ssl
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

def reader(pipe):
    result = queue.Queue()
    def consume():
        for line in pipe:
            result.put(line)
        result.put(None)
    threading.Thread(target=consume, daemon=True).start()
    return result

def next_line(lines, timeout=30):
    line = lines.get(timeout=timeout)
    if line is None:
        raise AssertionError("process ended before responding")
    return line

def start_host(exe, ttl, work, stderr):
    with socket.socket() as port_socket:
        port_socket.bind(("127.0.0.1", 0))
        port = port_socket.getsockname()[1]
    process = subprocess.Popen(
        [str(exe), "host", "--bind", "127.0.0.1", "--port", str(port),
         "--require-admin", "false", "--ttl-secs", str(ttl)],
        stdout=subprocess.PIPE, stderr=stderr, text=True, encoding="utf-8", cwd=work)
    try:
        invite = next_line(reader(process.stdout)).strip()
        address, code = invite.rsplit(":", 1)
        assert len(code) == 16 and not invite.startswith("wb1_"), "expected short invitation"
        assert any(c.isupper() for c in code) and any(c.islower() for c in code)
        assert any(c.isdigit() for c in code) and any(c in "-_!@#$%&*+=?" for c in code)
        endpoint = "https://" + address
        bootstrap = urllib.request.build_opener(urllib.request.ProxyHandler({}),
            urllib.request.HTTPSHandler(context=ssl._create_unverified_context()))
        nonce = base64.urlsafe_b64encode(os.urandom(32)).decode().rstrip("=")
        def post(opener, route, body):
            req = urllib.request.Request(endpoint + route, data=json.dumps(body).encode(),
                headers={"Content-Type": "application/json"})
            with opener.open(req, timeout=5) as response:
                return json.load(response)
        for attempt in range(30):
            try:
                challenge = post(bootstrap, "/v1/pair/challenge", {"client_nonce": nonce})
                break
            except urllib.error.URLError:
                if attempt == 29:
                    raise
                time.sleep(0.05)
        def proof(domain):
            fields = [domain, nonce, challenge["server_nonce"], challenge["endpoint"],
                challenge["certificate_pem"], str(challenge["expires_unix"])]
            payload = b"".join(struct.pack(">I", len(f.encode())) + f.encode() for f in fields)
            return base64.urlsafe_b64encode(hmac.new(code.encode(), payload, hashlib.sha256).digest()).decode().rstrip("=")
        assert challenge["endpoint"] == endpoint
        assert hmac.compare_digest(challenge["proof"], proof("winremote-server-proof-v1"))
        pinned = urllib.request.build_opener(urllib.request.ProxyHandler({}),
            urllib.request.HTTPSHandler(context=ssl.create_default_context(cadata=challenge["certificate_pem"])))
        finish = {"client_nonce": nonce, "server_nonce": challenge["server_nonce"],
            "proof": proof("winremote-client-proof-v1")}
        connection = post(pinned, "/v1/pair/finish", finish)
        assert connection["certificate_pem"] == challenge["certificate_pem"]
        try:
            post(pinned, "/v1/pair/finish", finish)
            raise AssertionError("pairing replay accepted")
        except urllib.error.HTTPError as error:
            assert error.code == 401
        return process, invite, connection
    except BaseException:
        process.kill()
        process.wait()
        raise

def stop(process):
    if process and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()

def requester(connection):
    context = ssl.create_default_context(cadata=connection["certificate_pem"])
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=context))
    def request(route, body=None, token=True, origin=None):
        headers = {}
        if token:
            headers["Authorization"] = "Bearer " + (connection["token"] if token is True else token)
        if origin:
            headers["Origin"] = origin
        data = None
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(connection["endpoint"] + route, data=data, headers=headers)
        try:
            with opener.open(req, timeout=15) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, None
    return request

def assert_dead(pid):
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.c_uint32]
    kernel.OpenProcess.restype = ctypes.c_void_p
    kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
    kernel.CloseHandle.argtypes = [ctypes.c_void_p]
    handle = kernel.OpenProcess(0x00100000, False, pid)
    if not handle:
        assert ctypes.get_last_error() in (87, 1168), "could not verify descendant termination"
        return
    try:
        assert kernel.WaitForSingleObject(handle, 5000) == 0, "descendant survived its command"
    finally:
        kernel.CloseHandle(handle)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--allow-headless", action="store_true", help="Allow screenshot failure on a headless CI runner")
    args = parser.parse_args()
    exe = args.exe.resolve()
    assert os.name == "nt", "host smoke test requires Windows"
    processes = []
    with tempfile.TemporaryDirectory(prefix="winremote-smoke-") as temporary:
        work = Path(temporary)
        with (work / "stderr.txt").open("w", encoding="utf-8") as diagnostics:
            try:
                host, invite, connection = start_host(exe, 240, work, diagnostics)
                processes.append(host)
                request = requester(connection)
                assert request("/v1/info", token=False)[0] == 401
                assert request("/v1/info", token="wrong")[0] == 401
                assert request("/v1/info", origin="https://example.invalid")[0] == 403
                status, info = request("/v1/info")
                assert status == 200 and info["os"] == "Windows"
                assert "execute" in info["capabilities"]
                # System roots must not trust the fresh self-signed host certificate.
                untrusted = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                try:
                    untrusted.open(connection["endpoint"] + "/v1/info", timeout=5)
                    raise AssertionError("unpinned TLS unexpectedly trusted host")
                except urllib.error.URLError as error:
                    assert isinstance(error.reason, ssl.SSLCertVerificationError)
                print("PASS HTTPS trust, authentication and Origin rejection")

                credential = work / "connection.json"
                pair = subprocess.run([str(exe), "pair", "--connection", str(credential)],
                    input=invite + "\n", capture_output=True, encoding="utf-8", timeout=15)
                assert pair.returncode == 0, "pairing failed"
                assert not pair.stdout.strip(), "pairing printed unexpected stdout"
                assert credential.exists()
                acl = subprocess.run(["icacls", str(credential)], capture_output=True, encoding="utf-8")
                assert acl.returncode == 0 and "(I)" not in acl.stdout, "credentials inherited ACLs"
                print("PASS pairing and private credential ACL")

                mcp = subprocess.Popen([str(exe), "mcp", "--connection", str(credential)],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=diagnostics,
                    text=True, encoding="utf-8", cwd=work)
                processes.append(mcp)
                responses = reader(mcp.stdout)
                identifier = 0
                def rpc(method, params=None, notification=False):
                    nonlocal identifier
                    identifier += 1
                    message = {"jsonrpc": "2.0", "method": method}
                    if not notification:
                        message["id"] = identifier
                    if params is not None:
                        message["params"] = params
                    mcp.stdin.write(json.dumps(message) + "\n")
                    mcp.stdin.flush()
                    if notification:
                        return None
                    reply = json.loads(next_line(responses, 150))
                    assert reply["id"] == identifier and "error" not in reply, "MCP protocol error"
                    return reply["result"]
                initialized = rpc("initialize", {
                    "protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": {"name": "winremote-smoke", "version": "1"}})
                assert initialized["protocolVersion"] == "2025-11-25"
                rpc("notifications/initialized", notification=True)
                names = {item["name"] for item in rpc("tools/list")["tools"]}
                assert names == {"windows_info", "powershell_execute", "file_read", "file_write", "directory_list", "bridge_connect", "desktop_screenshot", "file_upload", "file_download", "file_stat", "file_copy", "file_move", "directory_create", "desktop_move", "desktop_click", "desktop_scroll", "desktop_type", "desktop_key"}
                def call(name, arguments, failed=False):
                    result = rpc("tools/call", {"name": name, "arguments": arguments})
                    assert bool(result.get("isError", False)) == failed, "unexpected tool status"
                    return result.get("structuredContent", json.loads(result["content"][0]["text"]))
                saved = credential.read_bytes()
                wrong = invite[:-1] + ("A" if invite[-1] != "A" else "B")
                rejected = rpc("tools/call", {"name": "bridge_connect", "arguments": {"invitation": wrong}})
                assert rejected["isError"] and credential.read_bytes() == saved
                print("PASS short invitation, authenticated TLS identity, replay and wrong-code rejection")
                credential.unlink()
                connected = call("bridge_connect", {"invitation": invite})
                assert connected["connected"] and "token" not in connected
                assert connected["host"]["os"] == "Windows"
                assert call("windows_info", {})["os"] == "Windows"
                print("PASS MCP initialize, tools/list and HTTPS tools/call")

                assert request("/v1/desktop/screenshot", {}, token=False)[0] == 401
                assert request("/v1/desktop/screenshot", {}, origin="https://example.invalid")[0] == 403
                assert request("/v1/desktop/screenshot", {"max_width": 0})[0] == 400
                shot_result = rpc("tools/call", {"name": "desktop_screenshot", "arguments": {"max_width": 960}})
                if shot_result.get("isError"):
                    assert args.allow_headless, "desktop screenshot failed on interactive test host"
                    # Headless/locked CI machines cannot provide an interactive desktop.
                    assert "host returned HTTP" in shot_result["content"][0]["text"]
                    print("SKIP live desktop screenshot: host has no capturable interactive desktop")
                else:
                    shot = shot_result["structuredContent"]
                    image = next(c for c in shot_result["content"] if c["type"] == "image")
                    png_bytes = base64.b64decode(image["data"], validate=True)
                    assert image["mimeType"] == "image/png" and png_bytes.startswith(b"\x89PNG\r\n\x1a\n")
                    width, height = struct.unpack(">II", png_bytes[16:24])
                    assert width == shot["width"] <= 960 and height == shot["height"] > 0
                    assert "content_base64" not in shot
                    print("PASS real desktop screenshot over authenticated HTTPS and MCP image content")

                testfile = work / "binary.bin"
                binary = bytes(range(256)) + b"+/\x00\xff"
                encoded = base64.b64encode(binary).decode()
                call("file_write", {"path": str(testfile), "content_base64": encoded})
                assert testfile.read_bytes() == binary
                read = call("file_read", {"path": str(testfile)})
                assert base64.b64decode(read["content_base64"]) == binary
                duplicate = rpc("tools/call", {"name": "file_write",
                    "arguments": {"path": str(testfile), "content_base64": encoded}})
                assert duplicate["isError"]
                assert any(entry["name"] == "binary.bin" for entry in
                    call("directory_list", {"path": str(work)})["entries"])
                assert request("/v1/files/read", {"path": "relative.txt"})[0] == 400
                print("PASS binary files, overwrite protection, directory list and paths")

                for route in ("/v1/files/stat", "/v1/files/read-chunk", "/v1/files/upload/begin",
                              "/v1/files/upload/commit", "/v1/files/upload/abort", "/v1/files/copy",
                              "/v1/files/move", "/v1/files/mkdir"):
                    assert request(route, {}, token=False)[0] == 401
                    assert request(route, {}, origin="https://example.invalid")[0] == 403
                print("PASS authentication and Origin guards on all fileshare JSON routes")

                # Large files use binary chunks; MCP outputs only paths, size and digest.
                local_source = work / "local-source-large.bin"
                large_data = bytes(range(256)) * (32768 + 3)  # >8 MiB and not a whole number of chunks
                local_source.write_bytes(large_data)
                remote_large = work / "remote-large.bin"
                uploaded = call("file_upload", {"local_path": str(local_source), "remote_path": str(remote_large)})
                expected_hash = hashlib.sha256(large_data).hexdigest()
                assert remote_large.read_bytes() == large_data
                assert uploaded["size_bytes"] == len(large_data) and uploaded["sha256"] == expected_hash
                assert "content_base64" not in uploaded
                stat = call("file_stat", {"path": str(remote_large)})
                assert stat["size_bytes"] == len(large_data) and not stat["is_dir"]
                download = work / "downloaded-large.bin"
                downloaded = call("file_download", {"local_path": str(download), "remote_path": str(remote_large)})
                assert download.read_bytes() == large_data and downloaded["sha256"] == expected_hash
                # Neither destination is silently replaced when overwrite is omitted.
                rejected = rpc("tools/call", {"name": "file_upload", "arguments": {
                    "local_path": str(local_source), "remote_path": str(remote_large)}})
                assert rejected["isError"]
                assert remote_large.read_bytes() == large_data
                rejected = rpc("tools/call", {"name": "file_download", "arguments": {
                    "local_path": str(download), "remote_path": str(remote_large)}})
                assert rejected.get("isError") and download.read_bytes() == large_data
                failed_download = rpc("tools/call", {"name": "file_download", "arguments": {
                    "local_path": str(download), "remote_path": str(work / "missing.bin"), "overwrite": True}})
                assert failed_download.get("isError") and download.read_bytes() == large_data
                empty = work / "empty.bin"; empty.write_bytes(b"")
                remote_empty = work / "remote-empty.bin"
                call("file_upload", {"local_path": str(empty), "remote_path": str(remote_empty)})
                empty_copy = work / "empty-copy.bin"
                call("file_download", {"local_path": str(empty_copy), "remote_path": str(remote_empty)})
                assert empty_copy.read_bytes() == b""
                print("PASS >8 MiB upload/download, SHA256, empty files and overwrite/failure preservation")

                folder = work / "new-remote-folder"
                call("directory_create", {"path": str(folder)})
                copied = folder / "copied.bin"
                call("file_copy", {"source": str(remote_large), "destination": str(copied)})
                assert copied.read_bytes() == large_data
                moved = folder / "moved.bin"
                call("file_move", {"source": str(copied), "destination": str(moved)})
                assert not copied.exists() and moved.read_bytes() == large_data
                nested = folder / "nested"; nested.mkdir(); (nested / "text.txt").write_text("copied folder")
                tree = work / "folder-copy"
                call("file_copy", {"source": str(folder), "destination": str(tree), "recursive": True})
                assert (tree / "nested/text.txt").read_text() == "copied folder"
                assert (tree / "moved.bin").read_bytes() == large_data
                unchanged = rpc("tools/call", {"name": "file_copy", "arguments": {
                    "source": str(folder), "destination": str(tree), "recursive": True}})
                assert unchanged.get("isError")
                print("PASS remote mkdir, file/folder copy, move and destination collision protection")

                cli_remote = work / "cli-remote.bin"
                cli_upload = subprocess.run([str(exe), "upload", "--connection", str(credential),
                    "--local", str(local_source), "--remote", str(cli_remote)], capture_output=True, encoding="utf-8", timeout=30)
                assert cli_upload.returncode == 0 and json.loads(cli_upload.stdout)["sha256"] == expected_hash
                cli_download_path = work / "cli-download.bin"
                cli_download = subprocess.run([str(exe), "download", "--connection", str(credential),
                    "--local", str(cli_download_path), "--remote", str(cli_remote)], capture_output=True, encoding="utf-8", timeout=30)
                assert cli_download.returncode == 0 and cli_download_path.read_bytes() == large_data
                print("PASS command-line large-file transfer")

                def execute(script, timeout=10, failed=False):
                    return call("powershell_execute", {"script": script, "cwd": str(work),
                        "timeout_secs": timeout}, failed=failed)
                output = execute("Write-Output 'ciao Ã¨'; Write-Output (Get-Location).Path # comment")
                assert "ciao Ã¨" in output["stdout"] and str(work) in output["stdout"]
                assert execute("throw 'intentional test error'", failed=True)["exit_code"] == 1
                assert execute("cmd.exe /c exit 7", failed=True)["exit_code"] == 7
                overflow = execute("[Console]::Out.Write(('x' * 1100000)); [Console]::Error.Write(('y' * 1100000))")
                assert overflow["output_truncated"]
                assert len(overflow["stdout"].encode()) == 1048576
                assert len(overflow["stderr"].encode()) == 1048576
                timed = execute("Start-Sleep -Seconds 20", timeout=1, failed=True)
                assert timed["timed_out"]
                print("PASS PowerShell UTF-8, cwd, errors, exit code, output bounds and timeout")

                script = "$p = Start-Process -FilePath powershell.exe -ArgumentList '-NoProfile -NonInteractive -Command \"Start-Sleep -Seconds 60\"' -PassThru; [Console]::Out.WriteLine($p.Id); Start-Sleep -Seconds 20"
                tree = execute(script, timeout=2, failed=True)
                assert tree["timed_out"]
                child_pid = int(tree["stdout"].strip())
                assert_dead(child_pid)
                print("PASS descendant process termination")

                short, _, short_connection = start_host(exe, 2, work, diagnostics)
                processes.append(short)
                short.wait(timeout=15)
                assert short.returncode == 0
                assert short_connection["expires_unix"] <= time.time() + 1
                with socket.socket() as probe:
                    probe.settimeout(1)
                    assert probe.connect_ex(("127.0.0.1", int(short_connection["endpoint"].rsplit(":", 1)[1]))) != 0
                print("PASS invitation expiry closes listener")
                mcp.stdin.close()
                mcp.wait(timeout=10)
                assert mcp.returncode == 0
            finally:
                for process in reversed(processes):
                    stop(process)
    print("All local end-to-end checks passed; test processes and temporary credentials removed.")

if __name__ == "__main__":
    main()
