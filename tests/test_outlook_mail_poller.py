from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def test_outlook_sidecar_is_read_only_resumable_and_uploads_unicode_msg():
    source = (ROOT / "clients/outlook_mail_poller/LesMailPoller.cs").read_text(encoding="utf-8")

    assert "session.Stores" in source
    assert "new int[] { 3, 16, 23 }" in source
    assert "item.SaveAs(temp, 9)" in source
    assert "InternetMessageIdSchema" in source
    assert 'fields["store_id"]' in source
    assert 'fields["entry_id"]' in source
    assert "RegisterStore(storeId, storeLabel);" in source
    assert '"/collector/register-store"' in source
    assert source.index("RegisterStore(storeId, storeLabel);") < source.index(
        "ScanFolder(root, storeId, storeLabel"
    )
    assert "SaveCursor(storeId, folderId, cursor)" in source
    assert "incremental.Count - 1" in source
    assert "if (!RegisterItemAt(" in source
    assert "NewestEntryIds" in source
    assert "OldestEntryIds" in source
    assert "BackfillComplete" in source
    assert "cursor.BackfillComplete || registered >= BatchLimit || RunBudgetExceeded()" in source
    assert "cursor.BackfillComplete = true;" in source
    assert '--self-test-cursor' in source
    assert "CursorSelfTest()" in source
    assert "duration_ms=" in source
    assert "private const int BatchLimit = 10;" in source
    assert "RunBudgetMilliseconds = 12000" in source
    assert "HardStopMilliseconds = 15000" in source
    assert "Environment.Exit(0)" in source
    assert "run forced stop duration_ms=" in source
    assert "RunBudgetExceeded()" in source
    assert "if (!Register(" in source
    assert "GetItemFromID(entryId, storeId)" in source
    assert "item.Delete(" not in source
    assert "item.Move(" not in source
    assert ".UnRead =" not in source


def test_mail_has_a_dedicated_offline_and_windows_static_release_gate():
    makefile = (ROOT / "Makefile").read_text(encoding="utf-8")
    release = (ROOT / "tools/build_light_package.py").read_text(encoding="utf-8")

    assert "test-mail:" in makefile
    assert "test_mail_registry_service.py" in makefile
    assert "test_outlook_mail_poller.py" in makefile
    assert "def stage_mail_collector" in release
    assert "native/mail/LesLightMailPoller.exe" in release
