import hashlib

import httpx
import pytest

from proxy.services import light_update_service as updates


def manifest(body=b"MZ-test", **changes):
    return {"schema": "les.light-update.v1", "application_id": "me.ovc.les-light", "version": "0.1.1",
            "build_number": 999, "bytes": len(body), "sha256": hashlib.sha256(body).hexdigest(), **changes}


@pytest.mark.parametrize("changes", [{"application_id": "les.full"}, {"schema": "les.update.v1"}, {"bytes": True}, {"bytes": 2**40}, {"version": "../bad"}, {"sha256": "wrong"}])
def test_light_rejects_wrong_identity_and_invalid_manifest(changes):
    with pytest.raises(updates.LightUpdateError):
        updates.validate_manifest(manifest(**changes))


def test_update_url_is_derived_from_own_repo_not_remote_manifest():
    info = updates.validate_manifest(manifest(installer_url="https://example.com/full.exe"))
    assert info["installer_url"] == "https://github.com/proovcme/LES/releases/download/v0.1.1/LES-RAG-Setup.exe"
    assert not updates.validate_manifest(manifest(), current_version="0.1.2", current_build=1)["available"]


@pytest.mark.asyncio
async def test_missing_first_public_release_is_not_a_failure():
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(404))) as client:
        result = await updates.check_update(client=client)
    assert result["available"] is False
    assert "не опубликован" in result["message"]


@pytest.mark.asyncio
@pytest.mark.parametrize("body", [b"bad", b"MZ-test-and-extra"])
async def test_corruption_or_oversize_never_leaves_an_executable(tmp_path, body):
    info = updates.validate_manifest(manifest())
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(200, content=body))) as client:
        with pytest.raises(updates.LightUpdateError):
            await updates.download_installer(info, tmp_path, client)
    assert not list(tmp_path.rglob("*.exe"))
    assert not list(tmp_path.rglob("*.part"))


@pytest.mark.asyncio
async def test_verified_download_is_saved_without_installing(tmp_path):
    body = b"MZ-test"
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(200, content=body))) as client:
        path = await updates.download_installer(updates.validate_manifest(manifest(body)), tmp_path, client)
    assert path.read_bytes() == body
    assert path.name == "LES-RAG-Setup.exe"


@pytest.mark.asyncio
async def test_invalid_utf8_manifest_has_a_user_facing_error():
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(200, content=b'\xff'))) as client:
        with pytest.raises(updates.LightUpdateError, match='Проверьте интернет'):
            await updates.check_update(client=client)


@pytest.mark.asyncio
@pytest.mark.parametrize('failure', ['cancel', 'disconnect'])
async def test_interrupted_download_cleans_partial_and_allows_retry(tmp_path, failure):
    import asyncio
    body = b'MZ' + b'x' * (2 * 1024 * 1024)
    entered = asyncio.Event()
    class Interrupted(httpx.AsyncByteStream):
        async def __aiter__(self):
            yield body[:1024 * 1024]
            entered.set()
            if failure == 'disconnect':
                raise httpx.ReadError('synthetic disconnect')
            await asyncio.Event().wait()
    info = updates.validate_manifest(manifest(body))
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(200, stream=Interrupted()))) as client:
        task = asyncio.create_task(updates.download_installer(info, tmp_path, client))
        await asyncio.wait_for(entered.wait(), 2)
        if failure == 'cancel':
            assert next(tmp_path.rglob('*.part')).stat().st_size == 1024 * 1024
            task.cancel()
        with pytest.raises(asyncio.CancelledError if failure == 'cancel' else httpx.ReadError):
            await task
    assert not list(tmp_path.iterdir())
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(200, content=body))) as client:
        target = await updates.download_installer(info, tmp_path, client)
    assert target.read_bytes() == body
