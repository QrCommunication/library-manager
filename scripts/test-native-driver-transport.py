#!/usr/bin/env python3
"""Exercise a real tauri-driver proxy against a bounded synthetic local backend.

The first backend deliberately retains a socket after replying to a request with
Connection: close. Reusing that socket causes a reset; opening a new one succeeds.
The second backend resets after receiving a mutation and must receive it once.
No WebKit application, library profile, provider, or physical device is involved.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time


PURPOSE = "native-driver-transport-synthetic-backend"
MODES = {"requestClose", "receivedReset"}
MAX_BODY = 4096
MAX_LOG = 65536


class FixtureFailure(RuntimeError):
    """Only fixed diagnostic codes reach the public report."""


def require(condition, code):
    if not condition:
        raise FixtureFailure(code)


class Backend(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, port, directory, mode):
        self.directory = directory
        self.mode = mode
        self.lock = threading.Lock()
        self.connection_count = 0
        self.request_count = 0
        super().__init__(("127.0.0.1", port), BackendHandler)

    def handle_error(self, request, client_address):
        # The deliberate RST may interrupt the handler's final flush. No raw
        # transport traceback or request body belongs in the fixture report.
        pass


class BackendHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def setup(self):
        super().setup()
        self.connection.settimeout(10)
        with self.server.lock:
            self.server.connection_count += 1
            self.connection_id = self.server.connection_count
        self.requests_on_connection = 0

    def log_message(self, *args):
        pass

    def reset_connection(self):
        self.close_connection = True
        self.connection.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
        self.connection.close()

    def do_POST(self):
        length = self.headers.get("Content-Length", "")
        if not length.isdecimal() or not 0 < int(length) <= MAX_BODY:
            self.send_error(400)
            return
        body = self.rfile.read(int(length))
        try:
            value = json.loads(body)
        except (ValueError, UnicodeError):
            self.send_error(400)
            return
        command = value.get("command") if isinstance(value, dict) else None
        if command not in {"first", "second", "mutation"}:
            self.send_error(400)
            return
        self.requests_on_connection += 1
        with self.server.lock:
            self.server.request_count += 1
            if self.server.request_count > 8:
                self.reset_connection()
                return
            event = {"command": command, "connectionId": self.connection_id,
                     "connectionCloseRequested": self.headers.get("Connection", "").lower() == "close"}
            with (self.server.directory / "received.jsonl").open("a", encoding="utf-8") as output:
                output.write(json.dumps(event) + "\n")
                output.flush()
                os.fsync(output.fileno())
        if self.server.mode == "receivedReset" or self.requests_on_connection > 1:
            self.reset_connection()
            return
        response = json.dumps({"value": command}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)
        self.wfile.flush()
        # Holding the socket makes prohibited reuse observable without racing
        # an EOF notification or adding a sleep between commands.
        self.close_connection = False


def run_backend(arguments):
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", "-p", type=int, required=True)
    parser.add_argument("--host", default="127.0.0.1")
    args = parser.parse_args(arguments)
    require(args.host == "127.0.0.1" and 0 < args.port < 65536, "backendMustBeLocal")
    directory = Path(os.environ["LM_TRANSPORT_FIXTURE_DIRECTORY"])
    require(directory.is_absolute() and directory.is_dir() and not directory.is_symlink(), "backendDirectoryInvalid")
    marker = json.loads((directory / "marker.json").read_text())
    require(marker == {"purpose": PURPOSE}, "backendMarkerInvalid")
    mode = os.environ["LM_TRANSPORT_FIXTURE_MODE"]
    require(mode in MODES, "backendModeInvalid")
    with Backend(args.port, directory, mode) as server:
        server.serve_forever(poll_interval=0.1)


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_for_ports(process, ports, deadline):
    while time.monotonic() < deadline:
        require(process.poll() is None, "proxyExitedBeforeReady")
        ready = True
        for port in ports:
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                    pass
            except OSError:
                ready = False
                break
        if ready:
            return
        time.sleep(0.02)
    raise FixtureFailure("proxyStartupTimedOut")


def command_once(port, command, deadline):
    remaining = deadline - time.monotonic()
    require(remaining > 0, "fixtureDeadlineExceeded")
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=min(3, remaining))
    try:
        connection.request("POST", "/session/synthetic/execute/sync",
                           json.dumps({"command": command}),
                           {"Content-Type": "application/json", "Connection": "close"})
        response = connection.getresponse()
        body = response.read(MAX_BODY + 1)
        require(response.status == 200 and len(body) <= MAX_BODY, "proxyResponseInvalid")
        require(json.loads(body) == {"value": command}, "proxyResponseMismatch")
        return True
    except (OSError, http.client.HTTPException, ValueError):
        return False
    finally:
        connection.close()


def stop_proxy(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=2)


def received(directory):
    log = directory / "received.jsonl"
    require(log.is_file() and log.stat().st_size <= MAX_LOG, "backendReceiptMissingOrTooLarge")
    return [json.loads(line) for line in log.read_text().splitlines()]


def run_case(driver, mode, timeout):
    result = {"name": mode, "status": "running"}
    with tempfile.TemporaryDirectory(prefix="native-driver-transport-") as temporary:
        directory = Path(temporary)
        (directory / "marker.json").write_text(json.dumps({"purpose": PURPOSE}))
        helper = directory / "backend"
        helper.write_text("#!" + sys.executable + "\nimport runpy\nrunpy.run_path(" +
                          repr(str(Path(__file__).resolve())) + ", run_name='__main__')\n")
        helper.chmod(0o700)
        proxy_port, native_port = free_port(), free_port()
        while native_port == proxy_port:
            native_port = free_port()
        environment = os.environ.copy()
        environment.update({"LM_TRANSPORT_FIXTURE_DIRECTORY": str(directory), "LM_TRANSPORT_FIXTURE_MODE": mode})
        deadline = time.monotonic() + timeout
        with (directory / "proxy.log").open("wb") as output:
            process = subprocess.Popen([str(driver), "--port", str(proxy_port), "--native-port", str(native_port),
                                        "--native-driver", str(helper)], env=environment,
                                       stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                wait_for_ports(process, (proxy_port, native_port), deadline)
                if mode == "requestClose":
                    responses = [command_once(proxy_port, command, deadline) for command in ("first", "second")]
                else:
                    responses = [command_once(proxy_port, "mutation", deadline)]
                events = received(directory)
                result.update({"responsesSucceeded": responses, "receivedCommands": events})
                require(all(event["connectionCloseRequested"] for event in events), "proxyLostConnectionCloseHeader")
                if mode == "requestClose":
                    require([event["command"] for event in events] == ["first", "second"], "commandsDuplicatedOrLost")
                    require(responses == [True, True], "closedConnectionWasReused")
                    require(events[0]["connectionId"] != events[1]["connectionId"], "commandsUsedSameNativeConnection")
                else:
                    require(responses == [False], "receivedResetWasHidden")
                    require([event["command"] for event in events] == ["mutation"], "receivedMutationWasRetried")
                result["status"] = "passed"
            except FixtureFailure as error:
                result.update({"status": "failed", "errorCode": str(error)})
            finally:
                stop_proxy(process)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--driver", "--driver-binary", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--timeout", type=float, default=15)
    args = parser.parse_args()
    report = {"schemaVersion": 1, "status": "failed", "checks": [], "network": "syntheticLoopbackOnly",
              "realProfilesUsed": 0, "paidApiCalls": 0, "physicalDeviceWrites": 0}
    try:
        require(os.name == "posix" and 3 <= args.timeout <= 30, "fixtureEnvironmentInvalid")
        driver = args.driver.resolve(strict=True)
        require(driver.is_file() and os.access(driver, os.X_OK), "driverNotExecutable")
        report["driverSha256"] = hashlib.sha256(driver.read_bytes()).hexdigest()
        report["checks"] = [run_case(driver, mode, args.timeout) for mode in ("requestClose", "receivedReset")]
        report["status"] = "passed" if all(check["status"] == "passed" for check in report["checks"]) else "failed"
    except (FixtureFailure, OSError, ValueError, KeyError, subprocess.SubprocessError):
        report["errorCode"] = "transportFixtureInfrastructureFailed"
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps(report, sort_keys=True))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    if any(argument == "--port" or argument == "-p" or argument.startswith("--port=") for argument in sys.argv[1:]):
        run_backend(sys.argv[1:])
    else:
        raise SystemExit(main())
