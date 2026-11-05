"""Own the Light API, UI and native Qdrant for desktop or browser use."""
from __future__ import annotations

import argparse
import faulthandler
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
from backend.runtime_events import RuntimeJournal
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
        "LES_LIGHT_LEGACY_UI_URL": f"http://127.0.0.1:{ui_port}/classic",
        "LES_LIGHT_SVELTE_URL": f"http://127.0.0.1:{api_port}/app/",
        "SOVUSHKA_UI_PORT": str(ui_port), "SOVUSHKA_UI_HOST": "127.0.0.1", "NICEGUI_STORAGE_PATH": str(state / ".nicegui"),
        "LES_STARTUP_MODEL_WARMUP": "false", "LES_STARTUP_BACKGROUND_MUTATIONS": "true",
        "LES_FORMAT_WORKER_MODE": "native"})
    return env


class LightStack:
    def __init__(self, root, state, executable, *, read_only=False, frontend='classic'):
        self.root, self.state = root, state
        self.qdrant = LightQdrantRuntime(executable, state)
        self.children = []
        self.logs = []
        self.api_port, self.ui_port = 0, 0
        self.instance_id = ""
        self.read_only = read_only
        if frontend not in {'classic', 'svelte'}:
            raise ValueError('Unknown frontend')
        self.frontend = frontend
        self.health_monitor = HealthMonitor()
        self.journal = None
        self.last_failure = None

    def spawn(self, command, environment, log_name):
        log = (self.state / "logs" / log_name).open("ab")
        self.logs.append(log)
        process = subprocess.Popen(owned_command(command), cwd=self.state, env=environment, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        self.children.append(process)
        if self.journal:
            self.journal.record('service_started', service='api' if log_name == 'api-console.txt' else 'ui', child_pid=process.pid)
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
        if self.frontend == 'svelte' and not (self.root / 'desktop/light/frontend/dist/index.html').is_file():
            raise RuntimeError('В установке нет нового интерфейса. Соберите frontend или запустите прежний Лес.')
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
                return (f"http://127.0.0.1:{self.api_port}/app/" if self.frontend == 'svelte'
                        else f"http://127.0.0.1:{self.ui_port}/classic")
            except RuntimeError:
                collision = any(process.poll() is not None and not port_is_free(port)
                                for process, port in ((api, self.api_port), (ui, self.ui_port)))
                self.stop()
                if not collision or attempt == 2:
                    raise
                # Restart the pair so UI cannot retain a stale API address.

    def failed(self):
        dead = [{'service': name, 'child_pid': child.pid, 'exit_code': child.poll()}
                for name, child in zip(('api', 'ui'), self.children) if child.poll() is not None]
        process = self.qdrant._process
        if process is None or process.poll() is not None:
            dead.append({'service': 'qdrant', 'child_pid': process.pid if process else None,
                         'exit_code': process.poll() if process else None})
        if dead:
            self.last_failure = {'reason': 'service_exit', 'services': dead}
            return True
        failed = self.health_monitor.failed(
            f'http://127.0.0.1:{self.api_port}/api/light/instance',
            f'http://127.0.0.1:{self.ui_port}/healthz', self.instance_id,
            self.qdrant.url, self.qdrant.api_key)
        if failed:
            self.last_failure = {'reason': 'health_probe_failed', 'services': self.health_monitor.details}
        return failed

    def stop(self):
        for process in reversed(self.children):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    if self.journal:
                        self.journal.record('service_forced_stop', child_pid=process.pid, reason='shutdown_timeout')
                    process.kill()
                    process.wait(timeout=8)
            if self.journal:
                self.journal.record('service_stopped', child_pid=process.pid, exit_code=process.poll())
        self.children.clear()
        for log in self.logs:
            log.close()
        self.logs.clear()
        process = self.qdrant._process
        self.qdrant.stop()
        if process is not None and self.journal:
            self.journal.record('service_stopped', service='qdrant', child_pid=process.pid, exit_code=process.poll())


def reopen_owned_browser(state):
    try:
        status=json.loads((state/'launcher-status.json').read_text(encoding='utf-8'))
        if status.get('phase') != 'ready' or not status.get('instance_id'):
            return False
        api=urlsplit(status['api_url']);ui=urlsplit(status['ui_url'])
        for url in (api,ui):
            if url.scheme != 'http' or url.hostname != '127.0.0.1' or not url.port or url.username or url.password or url.query or url.fragment:
                return False
        if api.path not in {'','/'} or ui.path not in {'/classic', '/app/'}:
            return False
        if ui.path == '/app/' and ui.netloc != api.netloc:
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
    journal = RuntimeJournal(state)
    journal.record('launcher_started')
    faulthandler.enable()
    job = None
    stop = threading.Event()
    stop_reason = {'reason': 'normal_exit'}
    stack = None
    exit_code = 0
    terminal_phase = 'stopped'
    terminal_message = 'LES RAG остановлен'

    def request_stop(reason, signal_number=None):
        if not stop.is_set():
            stop_reason.update(reason=reason)
            journal.record('stop_requested', reason=reason, signal=signal_number)
        stop.set()

    try:
        job = attach_lifetime_job()
        for name in ("data", "storage", "logs", "RAG_Content", "artifacts"):
            (state / name).mkdir(exist_ok=True)
        signal.signal(signal.SIGINT, lambda signum, _frame: request_stop('signal', signum))
        signal.signal(signal.SIGTERM, lambda signum, _frame: request_stop('signal', signum))
        if args.parent_pipe:
            def parent_input():
                command = sys.stdin.readline()
                request_stop('desktop_stop' if command.strip() == 'stop' else
                             'parent_pipe_eof' if not command else 'invalid_parent_command')
            threading.Thread(target=parent_input, daemon=True).start()
        stack = LightStack(root, state, Path(args.qdrant).resolve(), read_only=args.read_only, frontend=getattr(args, 'frontend', 'classic'))
        stack.journal = journal
        opened_url = ""
        restart_times = []
        while not stop.is_set():
            write_status(state, "starting", "Запускаю LES RAG…", launcher_pid=os.getpid())
            try:
                ui_url = stack.start(stop)
            except InterruptedError:
                break
            except Exception:
                stop_reason['reason'] = 'startup_failed'
                raise
            write_status(state, "ready", "LES RAG готов", launcher_pid=os.getpid(), instance_id=stack.instance_id,
                ui_url=ui_url, api_url=f"http://127.0.0.1:{stack.api_port}", qdrant_pid=stack.qdrant._process.pid,
                api_pid=stack.children[-2].pid, ui_pid=stack.children[-1].pid)
            journal.record('ready', instance_id=stack.instance_id, api_pid=stack.children[-2].pid,
                           ui_pid=stack.children[-1].pid, qdrant_pid=stack.qdrant._process.pid)
            if args.browser and opened_url != ui_url:
                webbrowser.open(ui_url)
                opened_url = ui_url
            while not stop.wait(1) and not stack.failed():
                pass
            if stop.is_set():
                break
            journal.record('recovery_requested', failure=stack.last_failure)
            write_status(state, "recovering", "Служба остановилась или перестала отвечать. Восстанавливаю приложение…",
                         launcher_pid=os.getpid(), failure=stack.last_failure)
            stack.stop()
            now = time.monotonic()
            restart_times = [stamp for stamp in restart_times if now - stamp < 300]
            restart_times.append(now)
            if len(restart_times) > 3:
                stop_reason['reason'] = 'recovery_limit'
                raise RuntimeError('Служба повторно завершилась. Откройте журнал и повторите запуск после устранения причины.')
            stop.wait(2)
        write_status(state, "stopping", "Закрываю LES RAG…", **stop_reason, launcher_pid=os.getpid())
    except Exception as error:
        exit_code, terminal_phase = 1, 'error'
        terminal_message = f'Компоненты Леса остановлены: {error}'
        if stop_reason['reason'] == 'normal_exit':
            stop_reason['reason'] = 'launcher_exception'
        journal.record('launcher_error', **stop_reason, error_type=type(error).__name__)
    finally:
        try:
            if stack:
                stack.stop()
        except Exception as error:
            exit_code, terminal_phase = 1, 'error'
            stop_reason['reason'] = 'cleanup_failed'
            terminal_message = 'Не удалось завершить остановку. Проверьте журнал запуска.'
            journal.record('cleanup_failed', error_type=type(error).__name__)
        finally:
            journal.record('launcher_stopped', **stop_reason, exit_code=exit_code)
            write_status(state, terminal_phase, terminal_message, **stop_reason,
                         launcher_pid=os.getpid(), exit_code=exit_code, run_id=journal.run_id)
            lock.release()
            journal.close()
        # job intentionally stays open until process exit, including abrupt termination.
    return exit_code


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
    parser.add_argument("--frontend", choices=('classic', 'svelte'), default='classic', help='Opt-in renderer; classic remains the rollback during migration')
    return run(parser.parse_args(argv))


if __name__ == "__main__":
    raise SystemExit(main())
