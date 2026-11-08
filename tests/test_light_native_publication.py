"""Exercise the Rust writer and its crash journal against the shipping Qdrant server."""
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import struct
import subprocess
from uuid import uuid4

import httpx
import pytest
from qdrant_client import QdrantClient, models

from backend.document_catalog import MetaDB
from backend.index_replacement import ReplacementJournal
from backend.light_qdrant_runtime import LightQdrantRuntime
from proxy.services.lexical_index_service import LexicalIndex
from tests.test_light_native_api import free_port, wait_for


def test_native_publication_and_recovery_preserve_the_file_generation(tmp_path):
    executable = os.getenv("LES_TEST_NATIVE_API")
    qdrant = os.getenv("LES_TEST_NATIVE_QDRANT")
    if not executable or not qdrant:
        pytest.skip("Set LES_TEST_NATIVE_API and LES_TEST_NATIVE_QDRANT")
    state = tmp_path / "Состояние Леса"
    state.mkdir()
    spools = state / "data/index-publish"
    spools.mkdir(parents=True)
    database = state / "data/meta.db"
    MetaDB(str(database))
    with closing(LexicalIndex(str(database)).connect()):
        pass
    source = tmp_path / "Проект 東京.txt"
    source.write_bytes("Кабель в лотке".encode())
    stat = source.stat()
    name = source.name
    ticket = ["doc", str(source), stat.st_mtime, stat.st_size]
    with sqlite3.connect(database) as conn:
        conn.execute("INSERT INTO datasets(id,name) VALUES('ds','Проект')")
        conn.execute("INSERT INTO documents(id,dataset_id,file_name,status,source_path,file_mtime,file_size) "
                     "VALUES(?, 'ds', ?, 'PENDING', ?, ?, ?)", [ticket[0], name, *ticket[1:]])

    runtime = LightQdrantRuntime(Path(qdrant), state)
    token = "1" * 64
    listen, upstream = free_port(), free_port()
    while upstream == listen:
        upstream = free_port()
    process = None
    try:
        runtime.start()
        environment = {**os.environ, "LES_LIGHT_QDRANT_URL": runtime.url,
                       "LES_LIGHT_QDRANT_API_KEY": runtime.api_key, "LES_INTERNAL_INGEST_TOKEN": token,
                       "LES_STARTUP_BACKGROUND_MUTATIONS": "false"}
        def start_api():
            with (tmp_path / "api.log").open("ab") as log:
                return subprocess.Popen([executable, "--listen", f"127.0.0.1:{listen}", "--upstream",
                    f"http://127.0.0.1:{upstream}", "--state", str(state), "--database", str(database)],
                    env=environment, stdout=log, stderr=subprocess.STDOUT)

        process = start_api()
        journal = ReplacementJournal(state / "storage/datasets", "docs")
        with httpx.Client(base_url=f"http://127.0.0.1:{listen}", timeout=30, trust_env=False) as client, closing(
            QdrantClient(url=runtime.url, api_key=runtime.api_key, check_compatibility=False, timeout=20)
        ) as vectors:
            endpoint = "/api/light/internal-index-store"
            wait_for(lambda: client.post(endpoint).status_code == 404)
            vectors.create_collection("docs", vectors_config=models.VectorParams(size=2, distance=models.Distance.COSINE))
            headers = {"x-les-index-token": token}

            def publish(point_id, sha, text, *, extra_line="", doc_name=name, db_name=name, registration=ticket):
                spool = uuid4().hex
                point = {"id": point_id, "vector": [1, 0], "payload": {"text": text, "content_hash": "a" * 40,
                         "embedding_fingerprint": "fixture",
                         "dataset_id": "ds", "file_name": doc_name, "source_sha256": sha, "parent_id": "parent",
                         "chunk_ord": 1}}
                (spools / f"{spool}.jsonl").write_text(
                    json.dumps(point, ensure_ascii=False) + "\n" + extra_line, encoding="utf-8")
                return client.post(endpoint, headers=headers, json={"action": "publish_file", "collection": "docs",
                    "dataset_id": "ds", "doc_name": doc_name, "db_file_name": db_name, "spool_id": spool,
                    "registration": registration, "fingerprint": {"file_hash": sha, "file_mtime": registration[2],
                    "file_size": registration[3]}, "route": dict.fromkeys(("domain", "dataset_name", "doc_type",
                    "content_type", "complexity", "pipeline"), "text"), "ids": [point_id], "verify_count": True})

            def index_state():
                return {str(point.id) for point in vectors.scroll("docs", limit=10)[0]}

            def catalog_state():
                with sqlite3.connect(database) as conn:
                    return (conn.execute("SELECT status,chunk_count,error_code FROM documents").fetchone(),
                            conn.execute("SELECT point_id,parent_id FROM lexical_chunks").fetchall())

            sha = hashlib.sha256(source.read_bytes()).hexdigest()
            old, new, staged = [str(uuid4()) for _ in range(3)]
            with journal.lease():
                assert publish(old, sha, "первый текст").json() == {"detail": "INDEX_UPDATE_BUSY"}
            assert publish(old, sha, "первый текст").raise_for_status().json()["points"] == 1
            cache_id = uuid4().hex
            cache_request = {"action": "prepare_cache", "collection": "docs", "dataset_id": "ds",
                "file_name": name, "spool_id": cache_id, "dimension": 2, "embedding_fingerprint": "fixture"}
            assert client.post(endpoint, json=cache_request).status_code == 404
            cache = client.post(endpoint, headers=headers, json=cache_request)
            assert cache.raise_for_status().json()["cached"] == 1
            assert (spools / f"{cache_id}.cache").read_bytes() == (
                b"LESCACH1" + struct.pack("<I", 2) + b"a" * 40 + struct.pack("<2f", 1, 0))
            unrelated = client.post(endpoint, headers=headers, json={
                **cache_request, "spool_id": uuid4().hex, "embedding_fingerprint": "different model"})
            assert unrelated.raise_for_status().json()["cached"] == 0
            assert publish(staged, "0" * 64, "изменившийся источник").json() == {
                "detail": "SOURCE_CHANGED_DURING_INDEXING"}
            assert publish(staged, sha, "неполная публикация", extra_line="{}\n").json() == {
                "detail": "INDEX_PUBLICATION_INVALID"}
            assert index_state() == {old}
            assert catalog_state() == (("INDEXED", 1, ""), [(old, "parent")])
            assert journal.pending() is None

            with sqlite3.connect(database) as conn:
                conn.execute("CREATE TRIGGER simulate_stop BEFORE UPDATE OF file_hash ON documents "
                             "BEGIN SELECT RAISE(ABORT,'fixture failure'); END")
            assert publish(new, sha, "принятая редакция").json() == {"detail": "INDEX_CATALOG_WRITE_FAILED"}
            assert journal.pending()["phase"] == "committing"
            assert index_state() == {new}
            assert catalog_state() == (("INDEXED", 1, ""), [(old, "parent")])

            process.kill()
            process.wait(timeout=10)
            process = start_api()
            wait_for(lambda: client.post(endpoint).status_code == 404)

            def recover():
                return client.post(endpoint, headers=headers, json={"action": "recover_file", "collection": "docs"})

            assert recover().raise_for_status().json() == {"status": "recovered"}
            assert recover().raise_for_status().json() == {"status": "idle"}
            assert index_state() == {new}
            assert catalog_state() == (("PENDING", 0, "INDEX_INTERRUPTED"), [(new, "parent")])
            assert journal.pending() is None
            assert source.read_bytes() == "Кабель в лотке".encode()
            assert source.stat().st_mtime_ns == stat.st_mtime_ns

            relative = Path("Раздел с пробелом") / "Внутренний 東京.txt"
            internal = state / "storage/datasets/ds" / relative
            internal.parent.mkdir(parents=True, exist_ok=True)
            internal.write_bytes(b"fixture")
            internal_stat = internal.stat()
            internal_ticket = ["internal", "", internal_stat.st_mtime, internal_stat.st_size]
            with sqlite3.connect(database) as conn:
                conn.execute("DROP TRIGGER simulate_stop")
                for doc_id, doc_name in [("internal", relative.name), ("occupied", relative.as_posix())]:
                    conn.execute("INSERT INTO documents(id,dataset_id,file_name,status,source_path,file_mtime,file_size) "
                                 "VALUES(?,'ds',?,'PENDING','',?,?)",
                                 (doc_id, doc_name, internal_stat.st_mtime, internal_stat.st_size))
            params = dict(doc_name=relative.as_posix(), db_name=relative.name, registration=internal_ticket)
            inner_id = str(uuid4())
            inner_sha = hashlib.sha256(b"fixture").hexdigest()
            assert publish(inner_id, inner_sha, "Новый документ", **params).json() == {
                "detail": "INDEX_SOURCE_ALIAS_CONFLICT"}
            assert index_state() == {new}
            with sqlite3.connect(database) as conn:
                conn.execute("DELETE FROM documents WHERE id='occupied'")
            assert publish(inner_id, inner_sha, "Новый документ", **params).raise_for_status().json()["points"] == 1
            with sqlite3.connect(database) as conn:
                assert conn.execute("SELECT file_name,status FROM documents WHERE id='internal'").fetchone() == (
                    relative.as_posix(), "INDEXED")
                assert conn.execute("SELECT doc_name FROM lexical_chunks WHERE point_id=?", (inner_id,)).fetchone() == (
                    relative.as_posix(),)
    finally:
        if process is not None:
            process.terminate()
            process.wait(timeout=10)
        runtime.stop()
