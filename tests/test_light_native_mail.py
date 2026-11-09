"""Native candidate mail and index run with no Python application or upstream."""
from contextlib import closing
import hashlib
import os
from pathlib import Path
import socketserver
import sqlite3
import subprocess
from threading import Event, Thread

import httpx
import pytest
from qdrant_client import QdrantClient

from tests.native_batch_fixtures import NativeBatchEmbeddings
from tests.test_light_native_api import free_port, wait_for
from tests.test_light_native_intake_execution import settings


def eml(uid, marker):
    return (f"From: fixture@example.invalid\r\nTo: test@example.invalid\r\n"
            f"Date: Sat, 03 Oct 2026 11:00:00 +0300\r\nSubject: Fixture {uid}\r\n"
            f"Message-ID: <message-{uid}@example.invalid>\r\n"
            "Content-Type: text/plain; charset=utf-8\r\n\r\n"
            + (marker + " Evidence for native mail indexing. ") * 16).encode()


class FakeImap:
    def __init__(self):
        self.messages = {1: eml(1, "First IMAP original"), 2: eml(2, "Second IMAP original")}
        self.commands = []
        self.arrived = Event()
        self.release = Event()
        self.pause = True
        owner = self

        class Handler(socketserver.StreamRequestHandler):
            def handle(self):
                try:
                    self.wfile.write(b"* OK fixture ready\r\n")
                    while raw := self.rfile.readline(8192):
                        tag, command = raw.rstrip(b"\r\n").split(b" ", 1)
                        owner.commands.append(command)
                        if command.startswith(b"LOGIN "):
                            assert command == b'LOGIN "fixture" "dummy fixture password"'
                        elif command == b'LIST "" "*"':
                            self.wfile.write(b'* LIST (\\HasNoChildren) "/" "INBOX"\r\n'
                                             b'* LIST (\\Junk) "/" "spam"\r\n')
                        elif command == b'EXAMINE "INBOX"':
                            self.wfile.write(b"* 2 EXISTS\r\n* OK [UIDVALIDITY 99] fixture\r\n")
                        elif command.startswith(b"UID SEARCH UID "):
                            start = int(command.split()[3].split(b":")[0])
                            # Some servers return the last UID for last+1:*.
                            ids = [i for i in owner.messages if i >= start] or [max(owner.messages)]
                            self.wfile.write(b"* SEARCH " + b" ".join(str(i).encode() for i in ids) + b"\r\n")
                        elif command.startswith(b"UID FETCH "):
                            uid = int(command.split()[2])
                            assert command.endswith(b"(UID BODY.PEEK[])")
                            body = owner.messages[uid]
                            if uid == 2 and owner.pause:
                                owner.arrived.set()
                                owner.release.wait(20)
                                return
                            if uid == 1:
                                self.wfile.write(f"* 1 FETCH (UID 1 BODY[] {{{len(body)}}}\r\n".encode()+body+b")\r\n")
                            else:
                                self.wfile.write(f"* 2 FETCH (BODY[] {{{len(body)}}}\r\n".encode()+body+b" UID 2)\r\n")
                        else:
                            raise AssertionError(command)
                        self.wfile.write(tag + b" OK completed\r\n")
                except (ConnectionError, OSError):
                    pass

        self.server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def make_msg(path):
    """Generate a Unicode compound file with the same stream layout as Outlook."""
    import ctypes as c
    import struct

    pointer = c.c_void_p
    dword = c.c_uint32
    ole = c.WinDLL("ole32")
    ole.StgCreateDocfile.argtypes = (c.c_wchar_p, dword, dword, c.POINTER(pointer))
    root = pointer()
    # STGM_CREATE | STGM_READWRITE | STGM_SHARE_EXCLUSIVE
    assert ole.StgCreateDocfile(str(path), 0x1012, 0, c.byref(root)) == 0

    def method(obj, slot, result, *args):
        table = c.cast(obj, c.POINTER(c.POINTER(pointer)))[0]
        return c.WINFUNCTYPE(result, pointer, *args)(table[slot])

    def release(obj):
        method(obj, 2, dword)(obj)

    def stream(parent, name, data):
        dest = pointer()
        make = method(parent, 3, c.c_int32, c.c_wchar_p, dword, dword, dword, c.POINTER(pointer))
        assert make(parent, name, 0x12, 0, 0, c.byref(dest)) == 0
        try:
            written = dword()
            write = method(dest, 4, c.c_int32, pointer, dword, c.POINTER(dword))
            assert write(dest, c.create_string_buffer(data), len(data), c.byref(written)) == 0
            assert written.value == len(data)
        finally:
            release(dest)

    def text(parent, tag, value):
        stream(parent, "__substg1.0_" + tag, (value + "\0").encode("utf-16-le"))

    try:
        props = bytearray(32)
        struct.pack_into("<I", props, 12, 1)
        struct.pack_into("<I", props, 20, 1)
        stream(root, "__properties_version1.0", bytes(props))
        text(root, "0037001F", "Письмо из Outlook")
        text(root, "1035001F", "<msg-fixture@example.invalid>")
        text(root, "1000001F", "Контрольный текст Outlook с проверяемой ссылкой. " * 16)
        attach = pointer()
        make_storage = method(root, 5, c.c_int32, c.c_wchar_p, dword, dword, dword, c.POINTER(pointer))
        assert make_storage(root, "__attach_version1.0_#00000000", 0x12, 0, 0, c.byref(attach)) == 0
        try:
            text(attach, "3707001F", "Записка.txt")
            stream(attach, "__substg1.0_37010102", b"Verified attachment bytes.")
        finally:
            release(attach)
        assert method(root, 9, c.c_int32, dword)(root, 0) == 0
    finally:
        release(root)
    return path.read_bytes()


def exercise(tmp_path, source_files=()):
    if os.name != "nt":
        pytest.skip("Credential Manager fixture requires Windows")
    program = os.getenv("LES_TEST_EDGE_API")
    if not program:
        pytest.skip("Build the Edge-enabled API")
    import pypdfium2_raw

    state = tmp_path / "state"
    database = state / "data/meta.db"
    database.parent.mkdir(parents=True)
    (state / ".env").write_text(
        "\n".join(f"{k}='{v}'" for k, v in settings().items())
        + "\nRAG_UPLOAD_SUFFIXES=.PDF,.XLS,.TXT\n", encoding="utf-8")
    port = free_port()
    url = f"http://127.0.0.1:{port}"
    key = "mail-843-edge-fixture"
    env = {k: v for k, v in os.environ.items() if not
           (k.startswith(("RAG_", "EMBED", "COREML_EMBED_", "LES_EXTERNAL_", "HF_"))
            or k in ("LES_ENV_PATH", "LES_EMBED_PROFILE", "BGE_MODEL", "LES_NATIVE_EMBEDDING_SPEC"))}
    env.update(LES_LIGHT_QDRANT_URL=url, LES_LIGHT_QDRANT_API_KEY=key,
               LES_INTERNAL_INGEST_TOKEN="9"*64, LES_STARTUP_BACKGROUND_MUTATIONS="true",
               LES_NATIVE_PDFIUM=str(Path(pypdfium2_raw.__file__).with_name("pdfium.dll")),
               PYTHONHOME=str(tmp_path/"no-python"), PYTHONPATH=str(tmp_path/"no-app"),
               PATH=str(tmp_path/"no-programs"))
    args = [program, "--listen", f"127.0.0.1:{port}", "--state", str(state), "--database", str(database),
            "--chat-owner", "rust", "--edge-storage", str(tmp_path/"edge")]
    api = None
    account = None
    embeddings = None
    imap = FakeImap()

    with httpx.Client(base_url=url, trust_env=False, timeout=30) as client:
        def start():
            nonlocal api
            with (tmp_path/"api.log").open("ab") as log:
                api = subprocess.Popen(args, env=env, stdout=log, stderr=log,
                                       creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
            wait_for(lambda: client.get("/api/mail/accounts").is_success, timeout=20)

        def get(path):
            return client.get(path).raise_for_status().json()

        def progress(account_id):
            return next(x for x in get("/api/mail/accounts")["accounts"] if x["id"] == account_id)

        def upload(original, entry, suffix="eml"):
            response = client.post("/api/mail/collector/import",
                                   data={"store_id": "store", "entry_id": entry, "folder_id": "in",
                                         "folder_path": "Входящие"},
                                   files={"message": ("original."+suffix, original)})
            assert response.headers["x-les-handler"] == "rust.mail"
            return response.raise_for_status().json()

        try:
            start()
            assert client.post("/api/mail/collector/register-store", data={"store_id": "store"},
                               headers={"Origin": url}).status_code == 403
            store = client.post("/api/mail/collector/register-store", data={"store_id": "store"}).raise_for_status().json()
            raw = eml(42, "Outlook transport fixture")
            outlook = upload(raw, "entry")
            assert outlook["account_id"] == store["account_id"]
            assert upload(raw, "entry")["created"] is False
            revision = upload(eml(42, "Changed revision from Outlook transport"), "entry")
            assert revision["message_id"] != outlook["message_id"]
            row = get("/api/mail/messages/"+outlook["message_id"])
            assert "Outlook transport fixture" in row["profile"]["text"]
            assert "raw_path" not in row
            msg_raw = make_msg(tmp_path/"outlook.msg")
            msg = upload(msg_raw, "msg-entry", "msg")
            msg_profile = get("/api/mail/messages/"+msg["message_id"])["profile"]
            assert msg_profile["metadata"]["extractor"] == "les.mail-msg.v1"
            assert "Контрольный текст Outlook" in msg_profile["text"]
            assert "Verified attachment bytes" in msg_profile["text"]
            assert msg_profile["metadata"]["attachments"][0]["sha256"] == hashlib.sha256(b"Verified attachment bytes.").hexdigest()
            account = client.post("/api/mail/accounts", json={"kind": "imap", "label": "IMAP fixture",
                "host": "127.0.0.1", "port": imap.server.server_address[1], "ssl": False,
                "login": "fixture", "password": "dummy fixture password"}).raise_for_status().json()["account"]
            assert account["secret_set"] and "password" not in str(account)
            aid = account["id"]
            route = f"/api/mail/accounts/{aid}"
            assert client.post(route+"/test").raise_for_status().json()["folders"] == 1
            assert client.post(route+"/sync", json={"max_messages": 2}).status_code == 202
            assert imap.arrived.wait(18), (tmp_path/"api.log").read_text(encoding="utf-8", errors="replace")
            assert get(f"/api/mail/messages?account_id={aid}")["count"] == 1
            api.kill()
            api.wait(timeout=10)
            imap.pause = False
            imap.release.set()
            start()
            assert progress(aid)["last_error"] == "MAIL_SYNC_INTERRUPTED"
            assert client.post(route+"/sync", json={"max_messages": 2}).status_code == 202
            wait_for(lambda: progress(aid)["sync_state"] == "idle", timeout=15)
            assert b"UID SEARCH UID 2:*" in imap.commands
            assert imap.commands.count(b"UID FETCH 1 (UID BODY.PEEK[])") == 1
            assert get(f"/api/mail/messages?account_id={aid}")["count"] == 2
            assert client.post(route+"/sync").status_code == 202
            wait_for(lambda: progress(aid)["sync_state"] == "idle", timeout=15)
            assert imap.commands.count(b"UID FETCH 2 (UID BODY.PEEK[])") == 2

            actual = []
            for n, path in enumerate(source_files):
                before = hashlib.sha256(path.read_bytes()).hexdigest()
                with path.open("rb") as f:
                    receipt = client.post("/api/rag/attach?mode=index",
                        headers={"Idempotency-Key": f"real-843-{n}"},
                        files={"file": (path.name, f)}).raise_for_status().json()
                actual.append((path, before, receipt))
            embeddings = NativeBatchEmbeddings(database)
            with closing(sqlite3.connect(database)) as db, db:
                db.execute("UPDATE native_intakes SET retry_at=0")
            def finished():
                with closing(sqlite3.connect(database)) as db:
                    return db.execute("SELECT count(*) FROM documents WHERE status='INDEXED' AND chunk_count>0").fetchone()[0] == 5 + len(actual)
            wait_for(finished, timeout=55)
            evidence = get("/api/mail/messages/"+outlook["message_id"])
            assert evidence["index_status"] == "INDEXED"
            resolved = client.get("/api/documents/by-id/"+evidence["rag_doc_id"]+"/raw",
                                  params={"source_sha256": hashlib.sha256(raw).hexdigest()}).raise_for_status()
            assert resolved.content == raw
            msg_evidence = get("/api/mail/messages/"+msg["message_id"])
            msg_original = client.get("/api/documents/by-id/"+msg_evidence["rag_doc_id"]+"/raw").raise_for_status()
            assert msg_original.content == msg_raw
            with closing(QdrantClient(url=url, api_key=key, check_compatibility=False)) as qdrant:
                points, _ = qdrant.scroll("docs", limit=1000, with_payload=True)
            assert any(p.payload.get("dataset_id") == outlook["dataset_id"] and
                       p.payload.get("source_sha256") == hashlib.sha256(raw).hexdigest() for p in points)
            for path, digest, receipt in actual:
                assert hashlib.sha256(path.read_bytes()).hexdigest() == digest
                resolved = client.get("/api/documents/by-id/"+receipt["doc_id"]+"/raw").raise_for_status()
                assert hashlib.sha256(resolved.content).hexdigest() == digest
            assert not list((state/"mail").rglob("*.pending"))
        finally:
            imap.close()
            try:
                if api is not None and api.poll() is None:
                    api.terminate()
                    api.wait(timeout=10)
            finally:
                if account is not None:
                    # This is a random fixture ID, never a configured mailbox.
                    from ctypes import WinDLL, get_last_error
                    vault = WinDLL("Advapi32", use_last_error=True)
                    assert vault.CredDeleteW("me.ovc.les-light.mail." + account["id"], 1, 0) or get_last_error() == 1168
                if embeddings is not None:
                    embeddings.close()


def test_native_mail_accounts_sync_resume_outlook_and_citations(tmp_path):
    exercise(tmp_path)
