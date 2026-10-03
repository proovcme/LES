from pathlib import Path

import pytest

from sovushka_ng import _canonical_workspace_tab


@pytest.mark.parametrize(
    ("requested", "expected"),
    [
        ("data", "data"),
        ("documents", "data"),
        ("datasets", "data"),
        ("mail", "mail"),
        ("studio", "chat"),
        ("cad_bim", "chat"),
        ("", "chat"),
    ],
)
def test_canonical_workspace_tab(requested, expected, monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    assert _canonical_workspace_tab(requested) == expected


def test_light_keeps_mail_route(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    assert _canonical_workspace_tab("mail") == "mail"


def test_legacy_redirect_preserves_the_complete_query_and_canonicalizes_storage():
    shell = Path("sovushka_ng.py").read_text(encoding="utf-8")

    assert "request.query_params.multi_items()" in shell
    assert 'if key != "tab"' in shell
    assert 'query_items.append(("tab", _canonical_tab))' in shell
    assert "urlencode(query_items, doseq=True)" in shell
    for old, current in (
        ("Документы", "Данные"),
        ("Датасеты", "Данные"),
        ("Студия", "AI ЧАТ"),
        ("CAD/BIM", "AI ЧАТ"),
    ):
        assert f'"{old}": "{current}"' in shell
    assert '"Почта": "Почта" if is_light() else "AI ЧАТ"' in shell


def test_shell_mounts_mail_only_when_edition_exposes_tab():
    shell = Path("sovushka_ng.py").read_text(encoding="utf-8")

    assert "build_data_workspace(is_admin=is_admin)" in shell
    assert '[(tab_mail, lambda: build_mail())] if tab_mail else []' in shell
    assert '[(tab_mail_settings, lambda: build_mail_settings())] if tab_mail_settings else []' in shell
    assert 'build_documents(surface="studio")' not in shell
    assert 'build_documents(surface="cad_bim")' not in shell
def test_light_catalog_does_not_provision_or_list_system_datasets(tmp_path, monkeypatch):
    from backend.qdrant_adapter import MetaDB
    from proxy.services.system_dataset_service import ensure_system_datasets
    monkeypatch.setenv('LES_PRODUCT_EDITION', 'light')
    db = MetaDB(str(tmp_path / 'meta.db'))
    with db._get_conn() as conn:
        assert ensure_system_datasets(conn) == []
        assert conn.execute('SELECT COUNT(*) FROM datasets').fetchone()[0] == 0
    user_id = db.create_dataset('SMETA_SERVICE_Index')
    with db._get_conn() as conn:
        conn.execute("INSERT INTO datasets(id,name,dataset_scope) VALUES('system','Service','system')")
    assert [d.id for d in db.list_datasets()] == [user_id]
    with db._get_conn() as conn:
        assert conn.execute('SELECT COUNT(*) FROM datasets').fetchone()[0] == 2
