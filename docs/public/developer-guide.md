# Development

Windows x64, Python 3.13, uv, Rust and NSIS.

```powershell
uv sync --frozen --extra mcp
make verify
make test
make test-mail
cargo build --locked --release --manifest-path desktop/light/src-tauri/Cargo.toml
```

See [release procedure](../RELEASE_PROCEDURE.md) and [architecture](../LIGHT_ARCHITECTURE.md). The installed application needs neither a development environment nor Docker.
