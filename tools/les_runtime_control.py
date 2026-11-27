"""Read-only host memory probe used by the Python regression tools."""

from __future__ import annotations

import csv
import os
import platform
import re
import subprocess
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


@dataclass(frozen=True)
class MemoryProcess:
    pid: int
    user: str
    rss_mb: float
    command: str
    les_owned: bool
    protected: bool


@dataclass(frozen=True)
class MemoryPreflight:
    ram_free_gb: float | None
    ram_total_gb: float | None
    swap_pct: float | None
    top_processes: list[MemoryProcess]
    kill_candidates: list[MemoryProcess]
    min_free_gb: float
    min_rss_mb: float


PROTECTED_PROCESS_NAMES = {
    "System",
    "Registry",
    "Memory Compression",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "services.exe",
    "lsass.exe",
    "winlogon.exe",
    "dwm.exe",
    "MsMpEng.exe",
    "kernel_task",
    "launchd",
    "WindowServer",
    "loginwindow",
    "sysmond",
    "powerd",
    "mDNSResponder",
    "securityd",
    "opendirectoryd",
    "trustd",
    "cfprefsd",
}

CREATE_NO_WINDOW = 0x08000000


def _subprocess_kwargs(platform_name: str | None = None) -> dict[str, object]:
    """Keep operational probes invisible on Windows."""
    active_platform = platform_name or os.name
    if active_platform == "nt":
        return {
            "creationflags": CREATE_NO_WINDOW,
            "stdin": subprocess.DEVNULL,
        }
    return {}


def _run(args: list[str], timeout: int = 20) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            args,
            capture_output=True,
            text=True,
            timeout=timeout,
            **_subprocess_kwargs(),
        )
    except FileNotFoundError as exc:
        return subprocess.CompletedProcess(args=args, returncode=127, stdout="", stderr=str(exc))


def _host_memory() -> tuple[float | None, float | None, float | None]:
    try:
        import psutil

        vm = psutil.virtual_memory()
        sw = psutil.swap_memory()
        return round(vm.available / 1e9, 1), round(vm.total / 1e9, 1), round(sw.percent, 1)
    except Exception:
        return None, None, None


def _process_name(command: str) -> str:
    first = command.strip().split(maxsplit=1)[0] if command.strip() else ""
    return Path(first).name or first


def _is_les_owned_process(command: str) -> bool:
    names = {"qdrant", "qdrant.exe", "les-api", "les-api.exe", "les-runtime", "les-runtime.exe", "les-light.exe"}
    return _process_name(command).lower() in names or str(ROOT) in command


def _is_protected_process(command: str) -> bool:
    if command.strip() in PROTECTED_PROCESS_NAMES:
        return True
    name = _process_name(command)
    if name in PROTECTED_PROCESS_NAMES:
        return True
    return any(f"/{protected}" in command for protected in PROTECTED_PROCESS_NAMES)


def _parse_ps_memory_processes(output: str | None) -> list[MemoryProcess]:
    processes: list[MemoryProcess] = []
    for line in (output or "").splitlines():
        parts = line.strip().split(None, 3)
        if len(parts) < 4:
            continue
        pid_raw, rss_raw, user, command = parts
        if not pid_raw.isdigit():
            continue
        try:
            rss_mb = int(rss_raw) / 1024
        except ValueError:
            continue
        pid = int(pid_raw)
        processes.append(
            MemoryProcess(
                pid=pid,
                user=user,
                rss_mb=round(rss_mb, 1),
                command=command,
                les_owned=_is_les_owned_process(command),
                protected=_is_protected_process(command) or pid == os.getpid(),
            )
        )
    return sorted(processes, key=lambda item: item.rss_mb, reverse=True)


def _parse_tasklist_memory_processes(output: str | None) -> list[MemoryProcess]:
    processes: list[MemoryProcess] = []
    for row in csv.reader((output or "").splitlines()):
        if len(row) < 5 or row[1].strip().lower() == "pid":
            continue
        image_name, pid_raw, _session_name, _session_num, mem_raw = row[:5]
        if not pid_raw.strip().isdigit():
            continue
        mem_digits = re.sub(r"[^\d]", "", mem_raw)
        if not mem_digits:
            continue
        pid = int(pid_raw.strip())
        rss_mb = int(mem_digits) / 1024
        command = image_name.strip()
        processes.append(
            MemoryProcess(
                pid=pid,
                user="",
                rss_mb=round(rss_mb, 1),
                command=command,
                les_owned=_is_les_owned_process(command),
                protected=_is_protected_process(command) or pid == os.getpid(),
            )
        )
    return sorted(processes, key=lambda item: item.rss_mb, reverse=True)


def memory_processes(limit: int = 10) -> list[MemoryProcess]:
    if platform.system().lower().startswith("win"):
        result = _run(["tasklist", "/FO", "CSV", "/NH"], timeout=8)
        if result.returncode != 0:
            return []
        return _parse_tasklist_memory_processes(result.stdout)[:limit]
    result = _run(["ps", "-axo", "pid=,rss=,user=,command="], timeout=8)
    if result.returncode != 0:
        return []
    return _parse_ps_memory_processes(result.stdout)[:limit]


def build_memory_preflight(
    *,
    limit: int = 10,
    min_rss_mb: float = 700.0,
    min_free_gb: float = 12.0,
) -> MemoryPreflight:
    ram_free_gb, ram_total_gb, swap_pct = _host_memory()
    top_processes = memory_processes(limit=limit)
    candidates = [
        process
        for process in top_processes
        if process.rss_mb >= min_rss_mb and not process.les_owned and not process.protected
    ]
    return MemoryPreflight(
        ram_free_gb=ram_free_gb,
        ram_total_gb=ram_total_gb,
        swap_pct=swap_pct,
        top_processes=top_processes,
        kill_candidates=candidates,
        min_free_gb=min_free_gb,
        min_rss_mb=min_rss_mb,
    )
