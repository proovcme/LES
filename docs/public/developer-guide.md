# Development

Prepare the locked environment once. Windows development uses Python 3.13,
uv, Rust, Make, and Node.js/npm. Installer builds also need NSIS.

```powershell
uv sync --frozen --extra mcp
git config --local core.hooksPath .githooks
make pre-push
```

| Check | Use |
| --- | --- |
| `make pre-push` | Fast, offline checks, documentation, public files, and retrieval contracts. The Git hook runs it for the outgoing commit. |
| `make verify` | Rust HTTP contract and test collection. Build the native API and core first. |
| `make test-frontend` | Frontend types and behavior. |
| `make test` | Python and native integration checks, including the pinned MCP SDK used by the sample servers. Build the frontend first and provide native binaries to exercise the associated scenarios. |
| `make test-mail` | Offline mail behavior. |

The full [Windows checks](../../.github/workflows/verify.yml) show which assets
are built and which environment variables point to them. In particular,
`LES_TEST_NATIVE_API` selects the Windows server build and `LES_TEST_EDGE_API`
selects the separate Edge build. Use the locked MCP extra for the subprocess
tests. Presence of a native asset determines whether its integration tests run.

## Push and merge

Start with a branch. The local hook requires a clean checkout of the outgoing
commit and stops the push if any fast check fails. GitHub then runs Rust
formatting, Windows behavior, desktop builds, and frontend checks. Merge only
after the current pull request has a passing **LES merge gate**. A newer push
cancels the superseded PR run.

The rule for `main` should require a pull request and the **LES merge gate** on
an up-to-date revision. Native compilation and tests share the Cargo output,
profile, and path remapping. The package workflow checks the exact installer
separately. [Release procedure](../RELEASE_PROCEDURE.md).

For retrieval changes, first inspect the actual request to the model. Compare
the selected dataset, excerpts, request count, source coordinates, and the
original opened from the answer. Run a new question and a question without
support as controls. A scripted provider exercises program behavior. Use a
live model to measure answer quality.
