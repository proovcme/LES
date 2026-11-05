# Workspace reference

The session/draft implementations are frozen from the repository before the
Rust migration (build 773). Only the draft module's import points at its paired
test module. They are a differential oracle, excluded from product packaging.
Do not update them to make a native mismatch disappear.

The project, advisory-note and conversation-context implementations are frozen
from the source immediately before build 777. Only storage-facing functions are
substituted during differential tests; model generation remains explicitly mocked
in the current service. These files are not a product fallback.
