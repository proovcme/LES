"""Own the Light API, UI and native Qdrant for desktop or browser use."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import secrets
import signal
import subprocess
import sys
import threading
import time
import webbrowser
from urllib.parse import urlsplit

import httpx

from backend.light_processes import InstanceLock, attach_lifetime_job, owned_command
from backend.light_health_monitor import HealthMonitor
from backend.light_qdrant_runtime import LightQdrantRuntime, free_port, port_is_free

ROOT = Path(__file__).resolve().parents[1]


def write_status(state, phase, message, **details):
    target = state / "launcher-status.json"
    temporary = target.with_suffix(".tmp")
    temporary.write_text(json.dumps({"schema": "les.light-launcher.v1", "phase": phase,
        "message": message, "updated_at": time.time(), **details}, ensure_ascii=False), encoding="utf-8")
    temporary.replace(target)


def child_environment(root, state, qdrant, api_port, ui_port, instance_id):
    # Installed Light never inherits another LES edition's paths or keys.
    system_keys = {"SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "TEMP", "TMP", "USERPROFILE", "LOCALAPPDATA", "APPDATA", "PROGRAMDATA", "PROGRAMFILES", "PROGRAMFILES(X86)", "COMMONPROGRAMFILES", "HOME", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE", "SHELL", "NUMBER_OF_PROCESSORS", "PROCESSOR_ARCHITECTURE", "HOMEDRIVE", "HOMEPATH"}
    env = {key: value for key, value in os.environ.items() if key.upper() in system_keys}
    env.update({"PYTHONPATH": str(root), "PYTHONUTF8": "1", "PYTHONNOUSERSITE": "1", "LES_PRODUCT_EDITION": "light",
        "LES_STATE_ROOT": str(state), "LES_WINDOWS_STATE_ROOT": str(state), "LES_ENV_PATH": str(state / ".env"),
        "LES_RUNTIME_HOME": str(root), "LES_REPO_ROOT": str(root), "RAG_META_DB_PATH": str(state / "data/meta.db"),
        "QDRANT_URL": qdrant.url, "LES_LIGHT_QDRANT_URL": qdrant.url, "LES_LIGHT_QDRANT_API_KEY": qdrant.api_key,
        "LES_LIGHT_INSTANCE_ID": instance_id, "PROXY_URL": f"http://127.0.0.1:{api_port}", "LES_PROXY_URL": f"http://127.0.0.1:{api_port}",
        "SOVUSHKA_UI_PORT": str(ui_port), "SOVUSHKA_UI_HOST": "127.0.0.1", "NICEGUI_STORAGE_PATH": str(state / ".nicegui"),
        "LES_STARTUP_MODEL_WARMUP": "false", "LES_STARTUP_BACKGROUND_MUTATIONS": "true"})
    return env


class LightStack:
    def __init__(self, root, state, executable, *, read_only=False):
        self.root, self.state = root, state
        self.qdrant = LightQdrantRuntime(executable, state)
        self.children = []
        self.logs = []
        self.api_port, self.ui_port = 0, 0
        self.instance_id = ""
        self.read_only = read_only
        self.health_monitor = HealthMonitor()

    def spawn(self, command, environment, log_name):
        log = (self.state / "logs" / log_name).open("ab")
        self.logs.append(log)
        process = subprocess.Popen(owned_command(command), cwd=self.state, env=environment, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        self.children.append(process)
        return process

    def wait_ready(self, process, url, stop, *, timeout=75):
        deadline = time.monotonic() + timeout
        with httpx.Client(timeout=2, trust_env=False) as client:
            while not stop.is_set():
                if process.poll() is not None:
                    raise RuntimeError("Служба Light завершилась при запуске. Откройте журнал запуска и повторите попытку.")
                try:
                    response = client.get(url)
                    if response.status_code == 200 and response.json().get("instance_id") == self.instance_id:
                        return
                except (httpx.HTTPError, ValueError):
                    pass
                if time.monotonic() >= deadline:
                    raise TimeoutError("Служба Light не успела запуститься. Проверьте доступ к папке приложения и защитное ПО.")
                stop.wait(0.2)
        raise InterruptedError("Запуск отменён")

    def start(self, stop):
        self.health_monitor = HealthMonitor()
        for attempt in range(3):
            self.qdrant.start()
            self.instance_id = secrets.token_urlsafe(24)
            self.api_port = self.api_port if self.api_port and port_is_free(self.api_port) else free_port()
            self.ui_port = self.ui_port if self.ui_port and port_is_free(self.ui_port) else free_port()
            while self.ui_port == self.api_port:
                self.ui_port = free_port()
            environment = child_environment(self.root, self.state, self.qdrant, self.api_port, self.ui_port, self.instance_id)
            if self.read_only:
                environment["LES_STARTUP_BACKGROUND_MUTATIONS"] = "false"
            # UI imports and API initialization are independent. Start both before
            # waiting; reveal the desktop only after both prove instance ownership.
            api = self.spawn([sys.executable, "-B", "-m", "uvicorn", "proxy.app:app", "--host", "127.0.0.1", "--port", str(self.api_port)], environment, "api-console.txt")
            ui = self.spawn([sys.executable, "-B", str(self.root / "sovushka_ng.py")], environment, "ui-console.txt")
            try:
                self.wait_ready(api, f"http://127.0.0.1:{self.api_port}/api/light/instance", stop)
                self.wait_ready(ui, f"http://127.0.0.1:{self.ui_port}/healthz", stop)
                return f"http://127.0.0.1:{self.ui_port}/classic"
            except RuntimeError:
                collision = any(process.poll() is not None and not port_is_free(port)
                                for process, port in ((api, self.api_port), (ui, self.ui_port)))
                self.stop()
                if not collision or attempt == 2:
                    raise
                # Restart the pair so UI cannot retain a stale API address.

    def failed(self):
        if any(child.poll() is not None for child in self.children) or self.qdrant._process is None or self.qdrant._process.poll() is not None:
            return True
        return self.health_monitor.failed(
            f'http://127.0.0.1:{self.api_port}/api/light/instance',
            f'http://127.0.0.1:{self.ui_port}/healthz', self.instance_id,
            self.qdrant.url, self.qdrant.api_key)

    def stop(self):
        for process in reversed(self.children):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=8)
        self.children.clear()
        for log in self.logs:
            log.close()
        self.logs.clear()
        self.qdrant.stop()


def reopen_owned_browser(state):
    try:
        status=json.loads((state/'launcher-status.json').read_text(encoding='utf-8'))
        if status.get('phase') != 'ready' or not status.get('instance_id'):
            return False
        api=urlsplit(status['api_url']);ui=urlsplit(status['ui_url'])
        for url in (api,ui):
            if url.scheme != 'http' or url.hostname != '127.0.0.1' or not url.port or url.username or url.password or url.query or url.fragment:
                return False
        if api.path not in {'','/'} or ui.path != '/classic':
            return False
        with httpx.Client(timeout=3,trust_env=False) as client:
            probe=client.get(status['api_url']+'/api/light/instance')
            if probe.status_code != 200 or probe.json().get('instance_id') != status['instance_id']:
                return False
        webbrowser.open(status['ui_url'])
        return True
    except (OSError, ValueError, KeyError, httpx.HTTPError):
        return False


def run(args):
    root = Path(args.root).resolve()
    state = Path(args.state).resolve()
    state.mkdir(parents=True, exist_ok=True)
    lock = InstanceLock(state / "launcher.lock")
    if not lock.acquire():
        if args.browser and reopen_owned_browser(state):
            return 0
        return 10  # Existing owner; never overwrite its status or stop its children.
    job = None
    stop = threading.Event()
    stack = None
    try:
        job = attach_lifetime_job()
        for name in ("data", "storage", "logs", "RAG_Content", "artifacts"):
            (state / name).mkdir(exist_ok=True)
        signal.signal(signal.SIGINT, lambda *_: stop.set())
        signal.signal(signal.SIGTERM, lambda *_: stop.set())
        if args.parent_pipe:
            def parent_input():
                sys.stdin.readline()  # command or EOF on desktop crash
                stop.set()
            threading.Thread(target=parent_input, daemon=True).start()
        stack = LightStack(root, state, Path(args.qdrant).resolve(), read_only=args.read_only)
        opened_url = ""
        restart_times = []
        while not stop.is_set():
            write_status(state, "starting", "Запускаю LES RAG…", launcher_pid=os.getpid())
            try:
                ui_url = stack.start(stop)
            except InterruptedError:
                break
            except Exception as error:
                stack.stop()
                write_status(state, "error", str(error), launcher_pid=os.getpid())
                return 1
            write_status(state, "ready", "LES RAG готов", launcher_pid=os.getpid(), instance_id=stack.instance_id,
                ui_url=ui_url, api_url=f"http://127.0.0.1:{stack.api_port}", qdrant_pid=stack.qdrant._process.pid,
                api_pid=stack.children[-2].pid, ui_pid=stack.children[-1].pid)
            if args.browser and opened_url != ui_url:
                webbrowser.open(ui_url)
                opened_url = ui_url
            while not stop.wait(1) and not stack.failed():
                pass
            if stop.is_set():
                break
            write_status(state, "recovering", "Служба остановилась или перестала отвечать. Восстанавливаю приложение…", launcher_pid=os.getpid())
            stack.stop()
            now = time.monotonic()
            restart_times = [stamp for stamp in restart_times if now - stamp < 300]
            restart_times.append(now)
            if len(restart_times) > 3:
                write_status(state, "error", "Служба повторно завершилась. Откройте журнал и повторите запуск после устранения причины.")
                return 1
            stop.wait(2)
        write_status(state, "stopping", "Закрываю LES RAG…")
        return 0
    except Exception as error:
        write_status(state, "error", f"Не удалось запустить LES RAG: {error}")
        return 1
    finally:
        try:
            if stack:
                stack.stop()
        finally:
            lock.release()
        # job intentionally stays open until process exit, including abrupt termination.


def default_state() -> Path:
    if sys.platform == "darwin":
        return Path.home() / "Library/Application Support/LES Light"
    return Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "LES Light"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=str(ROOT))
    parser.add_argument("--state", default=str(default_state()))
    native = "qdrant.exe" if os.name == "nt" else "qdrant"
    parser.add_argument("--qdrant", default=str(ROOT / "native/qdrant" / native))
    parser.add_argument("--parent-pipe", action="store_true")
    parser.add_argument("--browser", action="store_true")
    parser.add_argument("--read-only", action="store_true", help="Disable background mutations for isolated acceptance")
    return run(parser.parse_args(argv))


if __name__ == "__main__":
    raise SystemExit(main())
