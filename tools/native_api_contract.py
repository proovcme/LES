
"""Check the public Rust API in a disposable profile with no Python upstream."""

import argparse
import json
from contextlib import closing, contextmanager
from http.client import HTTPConnection
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parents[1]
LEGACY = ROOT / "docs/generated/native-api-baseline.json"
OUTPUT = ROOT / "docs/generated/native-api-routes.json"


def executable():
    name = "les-api.exe" if os.name == "nt" else "les-api"
    return Path(os.environ.get("LES_TEST_NATIVE_API") or ROOT / "native/api" / name).resolve()


def _request(port, method, path, body=None, *, admitted=False):
    headers = {"Connection": "close", "Content-Type": "application/json"}
    if admitted:
        headers["Origin"] = f"http://127.0.0.1:{port}"
    else:
        # A forwarded external address must not gain Windows loopback trust.
        headers["X-Forwarded-For"] = "192.0.2.5"
    data = json.dumps(body).encode("utf-8") if body is not None else None
    with closing(HTTPConnection("127.0.0.1", port, timeout=5)) as connection:
        connection.request(method, path, body=data, headers=headers)
        response = connection.getresponse()
        status = response.status
        if not admitted and status != 501:
            return status, None
        return status, json.loads(response.read())


@contextmanager
def _native_api(program):
    with tempfile.TemporaryDirectory(prefix="les-native-api-contract-") as folder:
        state = Path(folder)
        (state / "data").mkdir()
        env_path = state / ".env"
        env_path.write_text("TRUSTED_NETWORKS=127.0.0.1/32\nTRUSTED_NETWORK_ROLE=admin\n", encoding="utf-8")
        env = {key: value for key, value in os.environ.items()
               if key.upper() in {"SYSTEMROOT", "WINDIR"}}
        env.update(LES_ENV_PATH=str(env_path), LES_STARTUP_BACKGROUND_MUTATIONS="false")
        for key in ("HOME", "USERPROFILE", "LOCALAPPDATA", "APPDATA", "TMP", "TEMP"):
            env[key] = folder

        with socket.socket() as reserve:
            reserve.bind(("127.0.0.1", 0))
            port = reserve.getsockname()[1]
        with (state / "api.log").open("w+b") as log:
            process = subprocess.Popen(
                [str(program), "--listen", f"127.0.0.1:{port}", "--chat-owner", "rust",
                 "--state", folder, "--database", str(state / "data/meta.db")],
                cwd=folder, env=env, stdout=log, stderr=subprocess.STDOUT,
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            )
            try:
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline and process.poll() is None:
                    try:
                        if _request(port, "GET", "/api/health")[0] == 200:
                            break
                    except OSError:
                        pass
                    time.sleep(0.05)
                else:
                    log.seek(0)
                    raise RuntimeError(f"Rust API did not start: {log.read(4096).decode('utf-8', errors='replace')}")
                yield port
            finally:
                if process.poll() is None:
                    process.terminate()
                process.wait(timeout=10)


def _sample_path(template):
    def value(parameter):
        name = parameter.group(1).split(":", 1)[0]
        return "1" if name in {"page", "row", "offset"} else "les-contract-missing"

    return re.sub(r"\{([^{}]+)\}", value, template)


def _capture_routes(port):
    legacy = json.loads(LEGACY.read_text(encoding="utf-8"))["routes"]
    pairs = {(row["method"], row["path"]) for row in legacy}
    # These public Rust routes are newer than the frozen compatibility snapshot.
    pairs.update({
        ("GET", "/api/light/frontend"),
        ("POST", "/api/rag/sources/call"),
        ("POST", "/api/documents/by-id/{id}/open-native"),
    })
    routes = []
    for method, template in sorted(pairs):
        # Exercise dispatch without valid authority, resources or mutation arguments.
        status, response = _request(port, method, _sample_path(template))
        if status != 501 or response.get("code") != "NATIVE_ROUTE_NOT_AVAILABLE":
            routes.append({"method": method, "path": template})
    return {"schema": "les.native-api-routes.v1", "probes": len(pairs), "routes": routes}


def _verify_behavior(port):
    def successful(method, path, body=None):
        status, result = _request(port, method, path, body, admitted=True)
        assert 200 <= status < 300, (method, path, status, result)
        return result

    assert _request(port, "GET", "/api/projects")[0] == 401
    assert successful("GET", "/api/projects") == {"projects": []}
    project = successful("POST", "/api/projects", {"name": "Native contract project"})
    assert project["id"] and project["name"] == "Native contract project"
    created = successful("POST", "/api/workspace/sessions", {"title": "Native contract chat"})
    session = created["session_id"]
    assert successful("GET", f"/api/workspace/sessions/{session}")["title"] == "Native contract chat"
    dataset = successful("POST", "/api/rag/datasets", {"name": "Native contract dataset"})
    assert dataset["id"] and dataset["name"] == "Native contract dataset"
    reply = successful("POST", "/api/chat", {"session_id": session, "question": "/help"})
    assert reply["answer"] and reply["history_id"] and reply["command"]["action"] == "help"
    history = successful("GET", f"/api/chat/history?session_id={session}")
    assert history[-1]["history_id"] == reply["history_id"]
    assert successful("GET", "/api/mcp") == {"connections": []}
    assert successful("GET", "/api/model-connections") == {
        "connections": [], "bindings": {"answer": None, "embeddings": None, "local_fallback": None},
    }
    for path in ("/api/smeta-unknown", "/api/rim/unknown"):
        status, result = _request(port, "GET", path)
        assert status == 501 and result["code"] == "NATIVE_ROUTE_NOT_AVAILABLE"


def capture(program):
    with _native_api(program) as port:
        routes = _capture_routes(port)
        _verify_behavior(port)
    return routes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--record", action="store_true")
    args = parser.parse_args()
    program = executable()
    if not program.is_file():
        parser.error("Build les-api or set LES_TEST_NATIVE_API")
    current = capture(program)
    if args.record:
        OUTPUT.write_text(json.dumps(current, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    else:
        expected = json.loads(OUTPUT.read_text(encoding="utf-8"))
        if current != expected:
            before = {(row["method"], row["path"]) for row in expected["routes"]}
            after = {(row["method"], row["path"]) for row in current["routes"]}
            print(f"Native API route drift. Removed {sorted(before - after)}. Added {sorted(after - before)}.")
            return 1
    print(f"Rust API contract verified. {len(current['routes'])}/{current['probes']} probes reached native handlers.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
