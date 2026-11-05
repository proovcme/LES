# Development

Windows x64, Python 3.13, uv, Rust and NSIS.

```powershell
uv sync --frozen --extra mcp
uv run python tools/build_native_core.py --output native/core
$env:LES_CORE_LIBRARY = (Resolve-Path native/core/les_core.dll).Path
make verify
make test
make test-mail
cargo build --locked --release --manifest-path desktop/light/src-tauri/Cargo.toml
```

See [release procedure](../RELEASE_PROCEDURE.md) and [architecture](../LIGHT_ARCHITECTURE.md). The installed application needs neither a development environment nor Docker.
