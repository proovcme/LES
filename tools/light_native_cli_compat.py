"""Keep existing Python entry-point settings working with the native executable."""
from pathlib import Path
import subprocess
import sys


def main():
    executable = Path(__file__).resolve().parents[2] / "native/runtime/les-runtime.exe"
    args = ["mcp"] if Path(__file__).stem == "light_mcp_server" else sys.argv[1:]
    try:
        return subprocess.call([str(executable), "--cli", *args])
    except OSError:
        print("В установке отсутствует нативная командная строка Леса.", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
