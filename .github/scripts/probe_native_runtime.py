"""Check an owned MCP runtime's startup/schema without invoking native tools."""

import argparse
import json
import os
from pathlib import Path
import platform
import queue
import subprocess
import tempfile
import threading
import time


def probe(binary, source, target, driver_version):
    """Use only initialize and tools/list; always stop the owned child runtime."""
    host_os = {"Windows": "win32", "Linux": "linux", "Darwin": "darwin"}[platform.system()]
    host_arch = {"AMD64": "x64", "x86_64": "x64", "arm64": "arm64", "aarch64": "arm64"}[
        platform.machine()
    ]
    if target != f"{host_os}-{host_arch}":
        raise ValueError("startup probe requires a matching native runner")
    env = {
        **os.environ,
        "CUA_DRIVER_RS_TELEMETRY_ENABLED": "false",
        "CUA_DRIVER_RS_UPDATE_CHECK": "false",
        "CUA_DRIVER_MCP_ENVELOPES": "1",
        "DO_NOT_TRACK": "1",
    }
    with tempfile.TemporaryDirectory(prefix="cua-runtime-probe-") as home:
        env.update(
            HOME=home,
            USERPROFILE=home,
            APPDATA=home,
            LOCALAPPDATA=home,
            XDG_CONFIG_HOME=home,
            XDG_CACHE_HOME=home,
            XDG_STATE_HOME=home,
        )
        with tempfile.TemporaryFile(mode="w+b") as stderr:
            child = subprocess.Popen(
                [str(binary.resolve()), "mcp", "--direct", "--embedded"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=stderr,
                text=True,
                encoding="utf-8",
                env=env,
            )
            lines = queue.Queue()

            def receive():
                for line in child.stdout:
                    lines.put(line)
                lines.put(None)

            threading.Thread(target=receive, daemon=True).start()

            def send(message):
                child.stdin.write(json.dumps(message) + "\n")
                child.stdin.flush()

            def request(identifier, method, params):
                send({"jsonrpc": "2.0", "id": identifier, "method": method, "params": params})
                deadline = time.monotonic() + 30
                while True:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TimeoutError(f"MCP {method} timed out")
                    line = lines.get(timeout=remaining)
                    if line is None:
                        raise RuntimeError("runtime stopped before MCP response")
                    message = json.loads(line)
                    if message.get("id") == identifier:
                        if "error" in message:
                            raise RuntimeError(f"MCP {method} error: {message['error']}")
                        return message["result"]

            try:
                initial = request(
                    1,
                    "initialize",
                    {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": {"name": "native-runtime-build-probe", "version": "1.0.0"},
                    },
                )
                if initial["serverInfo"] != {"name": "cua-driver", "version": driver_version}:
                    raise ValueError("built runtime version differs from source")
                envelope = (
                    initial.get("capabilities", {})
                    .get("experimental", {})
                    .get("ai.cua.driver.envelopes", {})
                )
                if envelope.get("version") != 1:
                    raise ValueError("EOF-aware MCP envelopes unavailable")
                send({"jsonrpc": "2.0", "method": "notifications/initialized"})
                tools = request(2, "tools/list", {})["tools"]
                names = [tool["name"] for tool in tools]
                for required in (
                    "list_apps",
                    "launch_app",
                    "list_windows",
                    "get_window_state",
                    "zoom",
                    "click",
                    "drag",
                ):
                    if required not in names:
                        raise ValueError(f"missing native tool: {required}")
                for tool in tools:
                    if tool.get("inputSchema", {}).get("type") != "object":
                        raise ValueError(f"invalid native input schema: {tool['name']}")
                return {
                    "source": source,
                    "target": target,
                    "server": initial["serverInfo"],
                    "transport": "upstream_mcp_envelopes",
                    "tools": names,
                    "schemas": tools,
                    "input_calls": 0,
                    "capture_calls": 0,
                    "limits": "Startup and advertised API only; not live GUI certification.",
                }
            except Exception as error:
                stderr.seek(0, os.SEEK_END)
                stderr.seek(max(0, stderr.tell() - 4000))
                diagnostic = stderr.read().decode("utf-8", errors="replace")
                raise RuntimeError(f"Native startup failed: {error}\n{diagnostic}") from error
            finally:
                child.stdin.close()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.terminate()
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait(timeout=5)
                child.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--source", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--driver-version", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = probe(args.binary, args.source, args.target, args.driver_version)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(
        json.dumps(
            {
                key: result[key]
                for key in ("source", "target", "server", "input_calls", "capture_calls")
            }
        )
    )


if __name__ == "__main__":
    main()
