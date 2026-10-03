"""Installer-only GitHub update channel, isolated from all full-LES patch engines."""
from __future__ import annotations

import asyncio
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

import httpx

from backend.light_processes import InstanceLock
from backend.runtime_paths import mutable_path
from proxy.services.version_service import BUILD_NUMBER, LES_VERSION

REPOSITORY = "proovcme/LES"
MANIFEST_URL = f"https://github.com/{REPOSITORY}/releases/latest/download/light-update.json"
ASSET = "LES-RAG-Setup.exe"
MAX_BYTES = 1024 * 1024 * 1024


class LightUpdateError(ValueError):
    pass


def validate_manifest(data, *, current_version=LES_VERSION, current_build=BUILD_NUMBER):
    if not isinstance(data, dict) or data.get("schema") != "les.light-update.v1" or data.get("application_id") != "me.ovc.les-light":
        raise LightUpdateError("Этот пакет обновления не предназначен для LES RAG.")
    version = str(data.get("version") or "")
    sha = str(data.get("sha256") or "")
    size, build = data.get("bytes"), data.get("build_number")
    if not re.fullmatch(r"\d+\.\d+\.\d+", version) or not re.fullmatch(r"[0-9a-f]{64}", sha):
        raise LightUpdateError("В описании обновления отсутствует корректная версия или контрольная сумма.")
    if type(size) is not int or not 1 <= size <= MAX_BYTES or type(build) is not int or build < 1:
        raise LightUpdateError("В описании обновления указан недопустимый размер или номер сборки.")
    newer = (tuple(map(int, version.split('.'))), build) > (tuple(map(int, current_version.split('.'))), current_build)
    return {"available": newer, "compatible": True, "package_complete": True,
            "latest_version": version, "build_number": build, "bytes": size, "sha256": sha,
            "installer_url": f"https://github.com/{REPOSITORY}/releases/download/v{version}/{ASSET}",
            "message": f"Доступен LES RAG {version}. Будет загружен и проверен установщик." if newer else "Установлена актуальная версия LES RAG."}


async def check_update(*, client=None):
    owned = client is None
    if owned:
        client = httpx.AsyncClient(timeout=30, follow_redirects=True)
    try:
        async with client.stream("GET", MANIFEST_URL) as response:
            if response.status_code == 404:
                return {"available": False, "message": "Публичный выпуск LES RAG пока не опубликован."}
            response.raise_for_status()
            body = bytearray()
            async for block in response.aiter_bytes():
                body.extend(block)
                if len(body) > 65536:
                    raise LightUpdateError("Описание обновления слишком большое.")
        return validate_manifest(json.loads(body))
    except (httpx.HTTPError, json.JSONDecodeError, UnicodeDecodeError) as error:
        raise LightUpdateError("Не удалось проверить GitHub. Проверьте интернет и повторите попытку.") from error
    finally:
        if owned:
            await client.aclose()


def root():
    return Path(mutable_path("artifacts/light-updates"))


def read_status():
    path = root() / "status.json"
    if not path.exists():
        return {"state": "idle", "message": "Обновление не запускалось."}
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {"state": "failed", "message": "Не удалось прочитать состояние обновления."}


async def download_installer(info, directory, client):
    directory.mkdir(parents=True, exist_ok=True)
    if shutil.disk_usage(directory).free < info["bytes"] + 50 * 1024 * 1024:
        raise LightUpdateError("Недостаточно места для установщика. Освободите место и повторите загрузку.")
    staging = Path(tempfile.mkdtemp(prefix="download-", dir=directory))
    partial = staging / "download.part"
    checksum, size = hashlib.sha256(), 0
    try:
        async with client.stream("GET", info["installer_url"]) as response:
            response.raise_for_status()
            with partial.open("xb") as stream:
                async for block in response.aiter_bytes(chunk_size=1024 * 1024):
                    size += len(block)
                    if size > info["bytes"]:
                        raise LightUpdateError("Размер установщика не совпадает с описанием выпуска.")
                    checksum.update(block)
                    stream.write(block)
        if size != info["bytes"] or checksum.hexdigest() != info["sha256"]:
            raise LightUpdateError("Установщик не прошёл проверку целостности. Повторите загрузку.")
        target = staging / ASSET
        partial.replace(target)
        return target
    except (OSError, httpx.HTTPError, LightUpdateError, asyncio.CancelledError):
        partial.unlink(missing_ok=True)
        staging.rmdir()
        raise


async def install_update():
    if os.name != "nt":
        raise LightUpdateError("Этот установщик предназначен для Windows.")
    directory = root()
    directory.mkdir(parents=True, exist_ok=True)
    lock = InstanceLock(directory / "update.lock")
    if not lock.acquire():
        raise LightUpdateError("Обновление уже загружается. Дождитесь завершения.")
    try:
        async with httpx.AsyncClient(timeout=90, follow_redirects=True) as client:
            info = await check_update(client=client)
            if not info.get("available"):
                raise LightUpdateError(info["message"])
            target = await download_installer(info, directory, client)
        # A visible installer is the user-requested next step. It checks app locks
        # and owns its transactional replacement; this API never patches runtime.
        subprocess.Popen([str(target)], cwd=target.parent, close_fds=True)
        status = {"state": "installer_started", "message": "Установщик открыт. Закройте LES RAG и продолжите установку в его окне. Документы сохраняются отдельно от приложения.",
                  "version": info["latest_version"], "build_number": info["build_number"]}
        temporary = directory / "status.tmp"
        temporary.write_text(json.dumps(status, ensure_ascii=False), encoding="utf-8")
        temporary.replace(directory / "status.json")
        return status
    except (OSError, httpx.HTTPError) as error:
        raise LightUpdateError("Не удалось загрузить или открыть установщик. Проверьте интернет, место на диске и доступ к папке приложения.") from error
    finally:
        lock.release()
