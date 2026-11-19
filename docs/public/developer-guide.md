# Development

Windows x64, Python 3.13, uv, Rust, Node.js/npm and NSIS. Node is needed for
building the interface, not for the installed application.

```powershell
uv sync --frozen --extra mcp
uv run python tools/build_native_core.py --output native/core
$env:LES_CORE_LIBRARY = (Resolve-Path native/core/les_core.dll).Path
make frontend
make verify
make test
make test-mail
cargo build --locked --release --manifest-path desktop/light/src-tauri/Cargo.toml
```

`make verify-fast` checks formatting-independent static contracts and Python
syntax without built native assets. CI checks Rust formatting separately first.
Full `make verify` also captures the API contract and collects tests; it needs
the native core. `make test` validates the static frontend receipt before the
long suite, so a missing/stale interface fails immediately. The readiness check
does not install dependencies or rebuild anything.

See [release procedure](../RELEASE_PROCEDURE.md) and [architecture](../LIGHT_ARCHITECTURE.md). The installed application needs neither a development environment nor Docker.
