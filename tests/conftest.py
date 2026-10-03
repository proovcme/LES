"""Offline and reproducible defaults for the LES unit/integration test process."""

from __future__ import annotations

import os
from pathlib import Path


def pytest_configure(config):
    # pytest creates basetemp itself, but not a missing parent in a fresh clone.
    if config.option.basetemp:
        Path(config.option.basetemp).resolve().parent.mkdir(parents=True, exist_ok=True)


# Test collection/imports and health/config reads must not download models or
# depend on whether the workstation currently has internet access.
os.environ.setdefault("HF_HUB_OFFLINE", "1")
os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")
os.environ.setdefault("RAG_TOKENIZER_LOCAL_FILES_ONLY", "true")
