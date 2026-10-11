"""Own a native Light Qdrant process without attaching to another installation."""
from __future__ import annotations

import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import time

import httpx
from backend.light_processes import owned_command


class PortCollisionError(RuntimeError):
    """Another process took the selected port before Qdrant bound it."""


def free_port():
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        return reservation.getsockname()[1]


def port_is_free(port):
    with socket.socket() as reservation:
        if os.name != 'nt':
            # Match the server's reusable TCP bind: TIME_WAIT is not a live owner.
            reservation.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            reservation.bind(("127.0.0.1", port))
            return True
        except OSError:
            return False


def storage_path(path: Path) -> str:
    """Keep Windows mmap storage working beyond MAX_PATH, including UNC roots."""
    value = str(path.resolve())
    if os.name != "nt" or value.startswith("\\\\?\\"):
        return value
    if value.startswith("\\\\"):
        return "\\\\?\\UNC\\" + value[2:]
    return "\\\\?\\" + value


class LightQdrantRuntime:
    def __init__(self, executable: Path, state_root: Path, *, timeout: float = 30):
        self.executable = executable.resolve()
        self.root = state_root.resolve() / "qdrant"
        self.timeout = timeout
        self.url = ""
        self.api_key = ""
        self._process = None
        self._lock = None
        self._log = None
        self._config = None

    def _acquire(self):
        try:
            self.root.mkdir(parents=True, exist_ok=True)
            stream = (self.root / "runtime.lock").open("a+b")
        except PermissionError as exc:
            raise RuntimeError("Нет доступа к каталогу хранилища LES RAG. Проверьте права записи в папку данных приложения и ограничения защитного ПО.") from exc
        try:
            stream.seek(0, os.SEEK_END)
            if stream.tell() == 0:
                stream.write(b"0")
                stream.flush()
            stream.seek(0)
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as exc:
            stream.close()
            raise RuntimeError("Это хранилище LES RAG уже используется. Закройте другое окно приложения и повторите запуск.") from exc
        self._lock = stream

    def start(self):
        for attempt in range(3):
            try:
                return self._start_once()
            except PortCollisionError:
                if attempt == 2:
                    raise PortCollisionError("Не удалось выделить порт Qdrant после трёх попыток. Повторите запуск LES RAG.")

    def _start_once(self):
        if self._lock is not None:
            raise RuntimeError("Qdrant этого экземпляра уже запущен.")
        if not self.executable.is_file():
            raise FileNotFoundError("В установке LES RAG отсутствует Qdrant. Восстановите приложение установщиком; документы сохранятся.")
        self._acquire()
        try:
            port = free_port()
            self.url = f"http://127.0.0.1:{port}"
            self.api_key = secrets.token_urlsafe(32)
            self._config = self.root / "runtime.yaml"
            # JSON is valid YAML; no user-controlled YAML interpolation.
            self._config.write_text(json.dumps({
                "storage": {"storage_path": storage_path(self.root / "storage"), "snapshots_path": storage_path(self.root / "snapshots")},
                "service": {"host": "127.0.0.1", "http_port": port, "grpc_port": None, "api_key": self.api_key},
                "cluster": {"enabled": False}, "telemetry_disabled": True,
            }), encoding="utf-8")
            self._log = (self.root / "runtime.log").open("ab")
            try:
                self._process = subprocess.Popen(
                    owned_command([str(self.executable), "--config-path", str(self._config)]), cwd=self.root,
                    stdin=subprocess.DEVNULL, stdout=self._log, stderr=subprocess.STDOUT,
                    creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
                    env={key: value for key, value in os.environ.items() if not key.upper().startswith("QDRANT__")},
                )
            except OSError as exc:
                raise RuntimeError("Не удалось запустить Qdrant LES RAG. Восстановите приложение установщиком; документы сохранятся.") from exc
            deadline = time.monotonic() + self.timeout
            with httpx.Client(headers={"api-key": self.api_key}, timeout=1, trust_env=False) as probe:
                while time.monotonic() < deadline:
                    if self._process.poll() is not None:
                        if not port_is_free(port):
                            raise PortCollisionError("Выбранный порт занят другим приложением; выбираю новый.")
                        raise RuntimeError("Qdrant LES RAG завершился при запуске. Откройте диагностику хранилища и повторите запуск.")
                    try:
                        response = probe.get(self.url + "/collections")
                        protected = probe.get(self.url + "/collections", headers={"api-key": ""}) if response.status_code == 200 else None
                        if response.status_code == 200 and protected.status_code in {401, 403} and self._process.poll() is None:
                            return self
                    except httpx.TransportError:
                        pass
                    time.sleep(0.1)
            raise TimeoutError("Qdrant LES RAG не успел запуститься. Повторите запуск или откройте диагностику хранилища.")
        except PermissionError as exc:
            self.stop()
            raise RuntimeError("Нет доступа к файлам хранилища LES RAG. Проверьте права записи в папку данных приложения и ограничения защитного ПО.") from exc
        except BaseException:
            self.stop()
            raise

    def stop(self):
        if self._process is not None and self._process.poll() is None:
            self._process.terminate()
            try:
                self._process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                self._process.kill()
                self._process.wait(timeout=8)
        self._process = None
        if self._log is not None:
            self._log.close()
            self._log = None
        if self._config is not None:
            self._config.unlink(missing_ok=True)
            self._config = None
        self.api_key = ""
        self.url = ""
        if self._lock is not None:
            self._lock.close()
            self._lock = None

    def __enter__(self):
        return self.start()

    def __exit__(self, *_):
        self.stop()
