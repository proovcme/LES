"""Offline cross-encoder in a bounded, restartable local worker.

The optional runtime and model are explicitly configured. No package or model
download occurs in the chat process; a timed-out worker is terminated, including
its GPU allocation, before a subsequent request can start another one.
"""
from __future__ import annotations

import atexit
import importlib.util
import json
import logging
import math
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time


def configuration() -> tuple[str, str, str]:
    return (os.getenv("LES_RERANK_PYTHON", sys.executable).strip(),
            os.getenv("RERANK_MODEL", "BAAI/bge-reranker-v2-m3").strip(),
            os.getenv("RERANK_DEVICE", "cpu").strip())


def model_snapshot(model: str) -> Path | None:
    path = Path(model).expanduser()
    if not path.is_dir():
        cache = Path(os.getenv("HF_HUB_CACHE", str(Path.home() / ".cache/huggingface/hub")))
        repo = cache / ("models--" + model.replace("/", "--"))
        ref = repo / "refs/main"
        if not ref.is_file():
            return None
        revision = ref.read_text(encoding="utf-8").strip()
        if not revision or any(c not in "0123456789abcdef" for c in revision):
            return None
        path = repo / "snapshots" / revision
    required = ("config.json", "tokenizer_config.json", "tokenizer.json", "model.safetensors")
    if all((path / name).is_file() and (path / name).stat().st_size > 0 for name in required):
        return path.resolve()
    return None


def readiness() -> dict:
    python, model, device = configuration()
    runtime = Path(python)
    external = runtime.resolve() != Path(sys.executable).resolve()
    available = runtime.is_file() and (external or importlib.util.find_spec("sentence_transformers") is not None)
    snapshot = model_snapshot(model)
    reason = "ready" if available and snapshot else "runtime_missing" if not available else "model_missing"
    messages = {
        "ready": "Локальная модель найдена. Первый запуск займёт больше времени.",
        "runtime_missing": "Реранкер не подготовлен: отсутствует среда запуска. Отключите его, чтобы продолжить поиск.",
        "model_missing": "Модель реранкера не найдена на компьютере. Отключите его, чтобы продолжить поиск.",
    }
    return {"available": reason == "ready", "reason": reason, "model": model,
            "device": device, "detail": messages[reason]}


class Worker:
    def __init__(self, python: str, model: str, device: str):
        self.python, self.model, self.device = python, model, device
        self.lock = threading.Lock()
        self.process = None
        self.lines = queue.Queue()
        atexit.register(self.stop)

    def stop(self):
        process, self.process = self.process, None
        if process is not None:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)
            for stream in (process.stdin, process.stdout):
                if stream:
                    stream.close()

    def start(self):
        snapshot = model_snapshot(self.model)
        if snapshot is None:
            raise RuntimeError("RERANK_MODEL_MISSING")
        env = dict(os.environ, HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", PYTHONUTF8="1")
        env.pop("PYTHONPATH", None)
        self.lines = queue.Queue()
        self.process = subprocess.Popen(
            [self.python, "-u", str(Path(__file__).resolve()), "--worker", str(snapshot), self.device],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding="utf-8", env=env,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        def receive(process, lines):
            try:
                for line in process.stdout:
                    if line.startswith('{"les_rerank":'):
                        lines.put(line)
            finally:
                lines.put(None)
        threading.Thread(target=receive, args=(self.process, self.lines), daemon=True).start()
        def diagnostics(process):
            for line in process.stderr:
                logging.getLogger(__name__).info("[RERANK-WORKER] %s", line.rstrip()[:1000])
        threading.Thread(target=diagnostics, args=(self.process,), daemon=True).start()

    def score(self, pairs: list, *, timeout: float, batch_size: int) -> list[float]:
        started = time.monotonic()
        if not self.lock.acquire(timeout=timeout):
            raise TimeoutError("RERANK_BUSY")
        try:
            try:
                if self.process is None or self.process.poll() is not None:
                    self.stop()
                    self.start()
                    ready = self.lines.get(timeout=max(0.01, timeout - (time.monotonic() - started)))
                    if ready is None or not json.loads(ready).get("ready"):
                        raise RuntimeError("RERANK_WORKER_EXITED")
                self.process.stdin.write(json.dumps({"pairs": pairs, "batch_size": batch_size}, ensure_ascii=False) + "\n")
                self.process.stdin.flush()
                line = self.lines.get(timeout=max(0.01, timeout - (time.monotonic() - started)))
                if line is None:
                    raise RuntimeError("RERANK_WORKER_EXITED")
                result = json.loads(line)
                if result.get("error"):
                    raise RuntimeError(result["error"])
                scores = [float(value) for value in result["scores"]]
                if len(scores) != len(pairs) or not all(math.isfinite(value) for value in scores):
                    raise RuntimeError("RERANK_INVALID_SCORES")
                return scores
            except queue.Empty as exc:
                self.stop()
                raise TimeoutError("RERANK_TIMEOUT") from exc
            except BaseException:
                self.stop()
                raise
        finally:
            self.lock.release()


_workers: dict[tuple, Worker] = {}
_lock = threading.Lock()


def score_pairs(pairs: list, *, timeout: float, batch_size: int, model: str = "") -> list[float]:
    python, configured_model, device = configuration()
    key = (python, model or configured_model, device)
    with _lock:
        if key not in _workers:
            # A changed explicit configuration releases the previous model.
            for worker in _workers.values():
                with worker.lock:
                    worker.stop()
            _workers.clear()
            _workers[key] = Worker(*key)
        worker = _workers[key]
    return worker.score(pairs, timeout=timeout, batch_size=batch_size)


def _worker_main():
    # The desktop launcher deliberately strips unrelated environment variables;
    # PyTorch's default Windows cache lookup otherwise attempts Unix-only pwd.
    state = Path(os.getenv("LES_WINDOWS_STATE_ROOT", str(Path.home() / ".cache/les")))
    os.environ.setdefault("TORCHINDUCTOR_CACHE_DIR", str(state / "cache/reranker/torch"))
    from sentence_transformers import CrossEncoder
    model = CrossEncoder(sys.argv[2], device=sys.argv[3], max_length=512, local_files_only=True)
    print(json.dumps({"les_rerank": 1, "ready": True}), flush=True)
    for line in sys.stdin:
        try:
            request = json.loads(line)
            values = model.predict(request["pairs"], batch_size=request["batch_size"],
                                   show_progress_bar=False, convert_to_numpy=True)
            result = {"les_rerank": 1, "scores": [float(value) for value in values]}
        except Exception as exc:
            result = {"les_rerank": 1, "error": type(exc).__name__}
        print(json.dumps(result), flush=True)


if __name__ == "__main__" and "--worker" in sys.argv:
    _worker_main()
