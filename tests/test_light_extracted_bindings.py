"""Catch unresolved globals in modules split out of the former monoliths."""
import builtins
from pathlib import Path
import symtable

import pytest


@pytest.mark.parametrize("path", [
    "proxy/services/dataset_runtime.py",
    "proxy/services/dataset_parse_service.py",
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
