"""The Windows watch uses the same durable queue and publication owner as manual intake."""
from contextlib import closing
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sqlite3
import subprocess
from threading import Event, Thread
from time import sleep

import httpx
import pytest
from qdrant_client import QdrantClient
from qdrant_client.http.exceptions import UnexpectedResponse

from backend.document_catalog import MetaDB
from backend.light_qdrant_runtime import LightQdrantRuntime
from tests.native_batch_fixtures import NativeBatchEmbeddings
from tests.test_light_edge_migration import start, call
from tests.test_light_native_api import free_port, wait_for
from tests.test_light_native_batch_api import stop
from tests.test_light_native_intake_execution import settings


def wait(action, timeout=35):
    result = wait_for(action, timeout)
    assert result, f"Timed out: {action!r}"
    return result


class PublicationGate:
    """Stop after a real Qdrant write and before LES receives its result."""

    def __init__(self, qdrant_url):
        self.hash = None
        self.arrived = Event()
        self.release = Event()
        gate = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def forward(self):
                body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                headers = {name: self.headers[name] for name in ('Content-Type', 'api-key') if name in self.headers}
                response = httpx.request(self.command, qdrant_url + self.path, headers=headers,
                                         content=body, timeout=15, trust_env=False)
                if (response.is_success and self.command == 'PUT' and
                        self.path.split('?')[0] == '/collections/docs/points' and
                        gate.hash and gate.hash.encode() in body):
                    gate.arrived.set()
                    gate.release.wait(25)
                self.send_response(response.status_code)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(response.content)))
                try:
                    self.end_headers()
                    self.wfile.write(response.content)
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    pass

            do_GET = do_PUT = do_POST = do_DELETE = do_PATCH = forward

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.thread = Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self):
        return f'http://127.0.0.1:{self.server.server_port}'

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def revision(number, repeat=32):
    return (f'Controlled folder watch record {number}. Publication must preserve the original revision. ' * repeat).encode()


def digest(value):
    return hashlib.sha256(value).hexdigest()


def rewrite(path, content):
    before = path.stat()
    path.write_bytes(content)
    os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns))
    assert path.stat().st_mtime_ns == before.st_mtime_ns
    assert path.stat().st_size == before.st_size


@pytest.mark.parametrize('connection', ['local', 'windows-smb', 'edge'])
def test_native_folder_watch_resume_unavailability_and_publication_crash(tmp_path, connection):
    program = os.getenv('LES_TEST_NATIVE_API')
    qdrant = os.getenv('LES_TEST_NATIVE_QDRANT')
    if not program or not qdrant or not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Build the native API and provide a Qdrant binary')
    edge = tmp_path/'edge' if connection == 'edge' else None
    vector = os.getenv('LES_TEST_EDGE_VECTOR')
    if edge and not vector:
        pytest.skip('Build the Edge inspection helper')
    folder = tmp_path/'Папка 東京 с пробелами'
    folder.mkdir()
    if connection == 'windows-smb':
        if os.name != 'nt' or os.getenv('LES_TEST_SMB_LOOPBACK') != '1':
            pytest.skip('Opt in to the real Windows localhost C$ SMB share')
        assert folder.drive.casefold() == 'c:'
        root = Path(r'\\localhost\C$')/folder.relative_to(folder.anchor)
        assert root.is_dir(), 'The Windows SMB redirector must be able to read the isolated test directory'
    else:
        root = folder
    first, missing = folder/'План.txt', folder/'Приложение.txt'
    repeat = 1800 if edge else 32
    original, online, offline = (revision(n, repeat) for n in (1, 2, 3))
    first.write_bytes(original)
    missing.write_bytes(revision(9))
    assert (root/first.name).read_bytes() == original

    state = tmp_path/'state'
    database = state/'data/meta.db'
    database.parent.mkdir(parents=True)
    MetaDB(str(database))
    with closing(sqlite3.connect(database)) as db, db:
        db.execute("INSERT INTO datasets(id,name) VALUES('ds','Наблюдение')")
    (state/'.env').write_text('\n'.join(f"{key}='{value}'" for key, value in settings().items()) +
        f"\nLES_EXTERNAL_ALLOW_ANY=false\nLES_EXTERNAL_SOURCE_ROOTS='{root}'\nRAG_UPLOAD_SUFFIXES=.TXT\n"
        + ("RAG_UPSERT_BATCH=1\n" if edge else ""),
        encoding='utf-8')
    auth = tmp_path/'policy.json'
    auth.write_text(json.dumps({'schema':'les.native-auth-policy.v1','compatible':True,
        'trusted_role':'admin','proxy_header':'x-les-role','cors_origins':[],'cors_loopback':True}), encoding='utf-8')

    runtime = None if edge else LightQdrantRuntime(Path(qdrant), state)
    embeddings = NativeBatchEmbeddings(database)
    port, upstream = free_port(), free_port()
    owner = gate = None
    endpoint = '/api/light/folder-watches'
    api_url = f'http://127.0.0.1:{port}'
    edge_key = 'edge-watch-test'
    content = edge/'content' if edge else state/'storage/datasets'
    journal = content/'.index-replacements'/digest(b'docs')[:24]/'journal.sqlite'

    def launch():
        env = {key:value for key,value in os.environ.items()
               if not key.startswith(('RAG_','EMBED','COREML_EMBED_','LES_EXTERNAL_','HF_'))
               and key not in ('LES_ENV_PATH','LES_EMBED_PROFILE','BGE_MODEL','LES_NATIVE_EMBEDDING_SPEC')}
        env.update(LES_LIGHT_QDRANT_URL=api_url if edge else gate.url,
                   LES_LIGHT_QDRANT_API_KEY=edge_key if edge else runtime.api_key,
                   LES_INTERNAL_INGEST_TOKEN='8'*64, LES_STARTUP_BACKGROUND_MUTATIONS='true')
        args = [program,'--listen',f'127.0.0.1:{port}','--upstream',f'http://127.0.0.1:{upstream}',
                '--state',str(state),'--database',str(database),'--auth-policy-file',str(auth)]
        if edge:
            args.extend(['--edge-storage', str(edge)])
        with (tmp_path/'api.log').open('ab') as log:
            return subprocess.Popen(args, env=env, stdout=log, stderr=log,
                                    creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))

    def publication():
        with closing(sqlite3.connect(journal, timeout=1)) as db:
            return json.loads(db.execute('SELECT body FROM replacement WHERE id=1').fetchone()[0])

    def catalog_sha(name):
        with closing(sqlite3.connect(database, timeout=1)) as db:
            row = db.execute('SELECT file_hash FROM documents WHERE file_name LIKE ?', ('%/'+name,)).fetchone()
            return row[0] if row else None

    def lexical_ready():
        decision = publication()
        if decision['phase'] != 'idle':
            return False
        with closing(sqlite3.connect(journal.with_name('bm25.sqlite'))) as db:
            return db.execute('SELECT revision FROM corpus WHERE id=1').fetchone() == (decision['revision'],)

    def vector_hashes(name):
        url, key = (api_url, edge_key) if edge else (runtime.url, runtime.api_key)
        with closing(QdrantClient(url=url, api_key=key, check_compatibility=False)) as qdrant_client:
            def read():
                hashes, offset = set(), None
                try:
                    while True:
                        points, offset = qdrant_client.scroll('docs', limit=100, offset=offset, with_payload=True)
                        hashes.update(point.payload['source_sha256'] for point in points
                                      if point.payload.get('file_name','').endswith('/'+name))
                        if offset is None:
                            return hashes
                except UnexpectedResponse as error:
                    if not edge or error.status_code != 409 or b'INDEX_RECOVERY_REQUIRED' not in error.content:
                        raise
                    return None
            return wait(read)

    try:
        if runtime:
            runtime.start()
            gate = PublicationGate(runtime.url)
        owner = launch()
        with httpx.Client(base_url=f'http://127.0.0.1:{port}', trust_env=False, timeout=5) as client:
            wait(lambda: client.get(endpoint).status_code == 200)
            client.post('/api/rag/index-external', json={'path':str(root), 'dataset_id':'ds',
                        'background':True, 'watch':True}).raise_for_status()
            def overview():
                watches = client.get(endpoint).raise_for_status().json()['watches']
                return watches[0] if watches else {}
            watch_id = wait(overview)['id']

            def files():
                result = client.get(endpoint, params={'id':watch_id}).raise_for_status().json()['files']
                return {Path(file['file_name']).name:file['status'] for file in result}

            def scan_now(enabled=True):
                client.post(endpoint, json={'id':watch_id,'enabled':enabled}).raise_for_status()

            wait(lambda: files() == {first.name:'INDEXED', missing.name:'INDEXED'})
            assert vector_hashes(first.name) == {digest(original)}
            assert vector_hashes(missing.name) == {digest(revision(9))}

            # Verify online scheduling with equal size and mtime.
            rewrite(first, online)
            if connection != 'windows-smb':
                scan_now()
            wait(lambda: catalog_sha(first.name) == digest(online), timeout=75)
            wait(lambda: publication()['phase'] == 'idle')
            assert vector_hashes(first.name) == {digest(online)}
            stop(owner)
            owner = None

            # The next process must catch up automatically. Kill it while a real
            # vector mutation is visible and the durable decision is still staging.
            rewrite(first, offline)
            missing.rename(tmp_path/missing.name)
            if gate:
                gate.hash = digest(offline)
            owner = launch()
            if edge:
                wait(lambda: publication()['phase'] == 'staging')
                sleep(0.08)
            else:
                assert gate.arrived.wait(35), (tmp_path/'api.log').read_text(encoding='utf-8', errors='replace')[-2400:]
            assert publication()['phase'] == 'staging'
            assert catalog_sha(first.name) == digest(online)
            if gate:
                assert digest(offline) in vector_hashes(first.name)
            owner.kill()
            owner.wait(timeout=8)
            owner = None
            assert publication()['phase'] == 'staging'
            if edge:
                raw = start(Path(vector), 'serve-stdio', edge)
                try:
                    for source_hash in (digest(online), digest(offline)):
                        count = call(raw, 'count', {'filter':{'must':[{
                            'key':'source_sha256','match':{'value':source_hash},
                        }]}})['count']
                        assert count > 0, 'Both the previous and staged revision must exist at interruption'
                finally:
                    raw.stdin.close()
                    raw.wait(timeout=8)
            else:
                gate.hash = None
                gate.release.set()
            owner = launch()
            wait(lambda: catalog_sha(first.name) == digest(offline))
            wait(lambda: publication()['phase'] == 'idle')
            assert vector_hashes(first.name) == {digest(offline)}
            assert vector_hashes(missing.name) == {digest(revision(9))}
            wait(lexical_ready)
            assert files() == {first.name:'INDEXED', missing.name:'MISSING'}
            assert not list((state/'data/index-publish').iterdir())
            assert 'index_file_recovered' in (tmp_path/'api.log').read_text(encoding='utf-8')

            # The UNC test still uses the actual share and redirector. Move only
            # the synthetic test root so that this specific path is unreachable.
            before = len(embeddings.calls)
            hidden_folder = tmp_path/'offline-folder'
            folder.rename(hidden_folder)
            scan_now()
            wait(lambda: overview()['status'] == 'UNAVAILABLE')
            assert files() == {first.name:'INDEXED', missing.name:'MISSING'}
            hidden_folder.rename(folder)
            scan_now()
            wait(lambda: overview()['status'] == 'AVAILABLE')
            assert len(embeddings.calls) == before

            if os.name == 'nt':
                (tmp_path/missing.name).rename(missing)
                scan_now()
                wait(lambda: files().get(missing.name) == 'INDEXED')
                with first.open('r+b') as writing:
                    writing.seek(0)
                    writing.write(b'In progress')
                    writing.flush()
                    missing.rename(tmp_path/missing.name)
                    scan_now()
                    wait(lambda: overview()['status'] == 'STALE')
                    assert files().get(missing.name) == 'INDEXED'
                    assert catalog_sha(first.name) == digest(offline)
                    writing.seek(0)
                    writing.write(offline)
                    writing.flush()
            scan_now(False)
            assert overview()['status'] == 'PAUSED'
            first.rename(tmp_path/first.name)
            scan_now()
            wait(lambda: files() == {first.name:'MISSING', missing.name:'MISSING'})
            assert overview()['status'] == 'AVAILABLE'
            assert vector_hashes(first.name) == {digest(offline)}
            assert len(embeddings.calls) == before
    finally:
        if gate:
            gate.release.set()
        stop(owner)
        if gate:
            gate.close()
        if runtime:
            runtime.stop()
        embeddings.close()
