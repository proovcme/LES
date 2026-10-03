"""Catch unresolved globals in modules split out of the former monoliths."""
import builtins
from pathlib import Path
import symtable

import pytest


@pytest.mark.parametrize("path", [
    "proxy/services/chat_evidence_application_service.py",
    "proxy/services/dataset_runtime.py",
    "proxy/services/dataset_parse_service.py",
    "proxy/routers/dataset_catalog.py",
    "proxy/routers/dataset_document_ops.py",
    "proxy/routers/dataset_watch.py",
    "proxy/routers/dataset_external.py",
    "proxy/routers/dataset_cloud.py",
    "proxy/routers/dataset_uploads.py",
    "proxy/routers/dataset_parse.py",
    "proxy/routers/dataset_search.py",
    "sovushka/components/chat_artifacts.py",
    "sovushka/components/chat_rendering.py",
    "sovushka/components/document_browser.py",
    "sovushka/components/document_views.py",
    "sovushka/components/document_presentation.py",
])
def test_extracted_module_has_no_unresolved_globals(path):
    table = symtable.symtable(Path(path).read_text(encoding="utf-8"), path, "exec")
    defined = {
        symbol.get_name() for symbol in table.get_symbols()
        if symbol.is_assigned() or symbol.is_imported() or symbol.is_namespace()
    } | set(dir(builtins)) | {"__name__", "__file__", "__annotations__"}
    missing = set()

    def visit(scope):
        for symbol in scope.get_symbols():
            if symbol.is_global() and symbol.is_referenced() and symbol.get_name() not in defined:
                missing.add(symbol.get_name())
        for child in scope.get_children():
            visit(child)

    visit(table)
    assert not missing, f"{path}: unresolved globals {sorted(missing)}"
