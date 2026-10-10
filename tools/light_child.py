"""Supervise one macOS service and its descendants for the owning launcher."""
import os
import signal
import subprocess
import sys
import threading

import psutil


def run(parent_pid, command):
    if os.getppid() != parent_pid:
        return 10
    stop = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: stop.set())
    signal.signal(signal.SIGINT, lambda *_: stop.set())
    child = subprocess.Popen(command, stdin=subprocess.DEVNULL, start_new_session=True)
    try:
        while child.poll() is None:
            if stop.wait(.1) or os.getppid() != parent_pid:
                break
        return child.poll() or 0
    finally:
        # The service has its own session; cover children even if their parent
        # exited before psutil could enumerate them.
        try:
            os.killpg(child.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process = psutil.Process(child.pid)
            owned = process.children(recursive=True) + [process]
        except psutil.NoSuchProcess:
            owned = []
        for process in owned:
            try:
                process.terminate()
            except psutil.NoSuchProcess:
                pass
        _, alive = psutil.wait_procs(owned, timeout=3)
        for process in alive:
            try:
                process.kill()
            except psutil.NoSuchProcess:
                pass
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()


if __name__ == '__main__':
    raise SystemExit(run(int(sys.argv[1]), sys.argv[2:]))
