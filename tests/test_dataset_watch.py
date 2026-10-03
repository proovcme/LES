import pytest
from fastapi import HTTPException
from proxy.services.dataset_watch_service import DatasetWatcher


@pytest.mark.asyncio
async def test_watch_persists_logs_settles_and_submits_once(tmp_path):
    path = tmp_path / "watch.db"
    watch = DatasetWatcher(path)
    watch.configure("ds", tmp_path, enabled=True, auto_index=True)
    submitted = []

    async def check(settings):
        return {"_files": {"new": [{"file_name": "файл 🌲.txt", "mtime": 1}], "changed": [], "deleted": []}}

    async def sync(settings):
        submitted.append(settings["dataset_id"])
        return {"parse_started": True}

    await watch.tick(check, sync, now=100)
    await watch.tick(check, sync, now=105)
    assert submitted == []
    await watch.tick(check, sync, now=115)
    await watch.tick(check, sync, now=130)
    assert submitted == ["ds"]
    restored = DatasetWatcher(path).status("ds")
    assert restored["watch"]["status"] == "indexing"
    assert len([e for e in restored["events"] if e["kind"] == "new"]) == 1


@pytest.mark.asyncio
async def test_unavailable_folder_pauses_without_sync_and_requires_new_settle(tmp_path):
    watch = DatasetWatcher(tmp_path / "watch.db")
    watch.configure("ds", tmp_path, enabled=True, auto_index=True)
    unavailable = False
    submitted = []

    async def check(settings):
        if unavailable:
            raise HTTPException(409, "Нет доступа к папке")
        return {"_files": {"new": [], "changed": [], "deleted": [{"file_name": "old.txt"}]}}

    async def sync(settings):
        submitted.append(1)
        return {}

    await watch.tick(check, sync, now=100)
    unavailable = True
    await watch.tick(check, sync, now=115)
    await watch.tick(check, sync, now=130)
    assert not submitted
    assert len([event for event in watch.status("ds")["events"] if event["kind"] == "unavailable"]) == 1
    unavailable = False
    await watch.tick(check, sync, now=145)
    assert not submitted
    await watch.tick(check, sync, now=160)
    assert submitted == [1]


@pytest.mark.asyncio
async def test_observation_without_auto_index_never_mutates_dataset(tmp_path):
    watch = DatasetWatcher(tmp_path / "watch.db")
    watch.configure("ds", tmp_path, enabled=True, auto_index=False)

    async def check(settings):
        return {"_files": {"new": [{"file_name": "new.txt"}], "changed": [], "deleted": []}}

    async def forbidden_sync(settings):
        pytest.fail("observation must not start indexing")

    await watch.tick(check, forbidden_sync, now=100)
    await watch.tick(check, forbidden_sync, now=200)
    assert watch.status("ds")["watch"]["status"] == "changes"


@pytest.mark.asyncio
async def test_watch_ui_renders_file_names_as_text(monkeypatch):
    from nicegui import Client
    from nicegui.page import page
    from sovushka.components import dataset_watch

    async def get(_):
        return {"watch": {"status": "changes", "enabled": True}, "events": [{"at": 1, "message": "Добавлен", "file_name": "<script>.txt"}]}

    monkeypatch.setattr(dataset_watch, "api_get", get)
    with Client(page("/__watch_test")) as client:
        await dataset_watch.open_dataset_watch("ds", "C:/Docs")
        texts = [getattr(element, "text", "") for element in client.elements.values()]
        assert "Наблюдение за файлами" in texts
        assert any("<script>.txt" in text for text in texts)


@pytest.mark.asyncio
async def test_file_still_being_written_resets_settle_window(tmp_path):
    watch = DatasetWatcher(tmp_path / "watch.db")
    watch.configure("ds", tmp_path, enabled=True, auto_index=True)
    version = 1
    calls = []

    async def check(settings):
        return {"_files": {"new": [{"file_name": "upload.pdf", "size": version}], "changed": [], "deleted": []}}

    async def sync(settings):
        calls.append(1)
        return {}

    await watch.tick(check, sync, now=100)
    version = 2
    await watch.tick(check, sync, now=115)
    await watch.tick(check, sync, now=120)
    assert calls == []
    await watch.tick(check, sync, now=130)
    assert calls == [1]


@pytest.mark.asyncio
async def test_processing_status_does_not_resubmit_unchanged_source(tmp_path):
    watch = DatasetWatcher(tmp_path / 'watch.db')
    watch.configure('ds', tmp_path, enabled=True, auto_index=True)
    status, calls = 'INDEXED', []
    async def check(settings):
        return {'_files': {'new': [], 'changed': [{'file_name': 'same.txt', 'file_hash': 'new',
            'previous': {'status': status}}], 'deleted': []}}
    async def sync(settings):
        nonlocal status
        status = 'PENDING'
        calls.append(1)
        return {'parse_started': True}
    for stamp in (100, 115, 130, 145):
        await watch.tick(check, sync, now=stamp)
    assert calls == [1]
    assert watch.status('ds')['watch']['status'] == 'indexing'


def test_watch_connection_closes_on_success_and_failure(tmp_path):
    import sqlite3
    watch = DatasetWatcher(tmp_path / 'watch.db')
    with watch.connect() as successful:
        successful.execute('SELECT 1')
    with pytest.raises(sqlite3.ProgrammingError, match='closed'):
        successful.execute('SELECT 1')
    with pytest.raises(RuntimeError, match='synthetic'):
        with watch.connect() as failed:
            raise RuntimeError('synthetic')
    with pytest.raises(sqlite3.ProgrammingError, match='closed'):
        failed.execute('SELECT 1')


@pytest.mark.asyncio
async def test_independent_watchers_share_ownership_and_release_after_cancel(tmp_path):
    import asyncio

    path = tmp_path / "watch.db"
    first, second = DatasetWatcher(path), DatasetWatcher(path)
    first.configure("ds", tmp_path, enabled=True, auto_index=True)
    entered = asyncio.Event()
    calls = []

    async def blocked_check(settings):
        calls.append("first")
        entered.set()
        await asyncio.Event().wait()

    async def check(settings):
        calls.append("second")
        return {"_files": {"new": [], "changed": [], "deleted": []}}

    async def sync(settings):
        pytest.fail("no changes")

    running = asyncio.create_task(first.tick(blocked_check, sync))
    await entered.wait()
    await second.tick(check, sync)
    assert calls == ["first"]
    running.cancel()
    with pytest.raises(asyncio.CancelledError):
        await running
    await second.tick(check, sync)
    assert calls == ["first", "second"]


@pytest.mark.asyncio
async def test_overlapping_watch_ticks_do_not_duplicate_work(tmp_path):
    import asyncio
    watcher = DatasetWatcher(tmp_path / "watch.db")
    watcher.configure("ds", tmp_path, enabled=True, auto_index=True)
    entered = asyncio.Event()
    release = asyncio.Event()
    calls = []

    async def check(settings):
        calls.append(1)
        entered.set()
        await release.wait()
        return {"_files": {"new": [], "changed": [], "deleted": []}}

    async def sync(settings):
        pytest.fail("no changes")

    first = asyncio.create_task(watcher.tick(check, sync))
    await entered.wait()
    try:
        await watcher.tick(check, sync)
    finally:
        release.set()
        await first
    assert calls == [1]
