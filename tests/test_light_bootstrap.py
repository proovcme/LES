"""Exercise the actual PowerShell downloader against an isolated HTTP server."""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import subprocess
import sys
import threading

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / 'installers/windows/light/install-les.ps1'
pytestmark = pytest.mark.skipif(sys.platform != 'win32', reason='Windows bootstrap')
PAYLOAD = b'Acceptance fixture, never executed.\x00' * 4096


def manifest(**changes):
    return dict(schema='les.light-update.v1', application_id='me.ovc.les-light',
                version='0.1.0', build_number=741, bytes=len(PAYLOAD),
                sha256=hashlib.sha256(PAYLOAD).hexdigest()) | changes


def quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def run(script):
    return subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command',
                           f". {quote(SCRIPT)}; try {{ {script} }} catch {{ Write-Output $_; exit 1 }}"],
                          capture_output=True, timeout=25)


@pytest.fixture
def server():
    responses = {}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            body, status, length = responses.get(self.path, (b'', 404, None))
            self.send_response(status)
            if length is not None:
                self.send_header('Content-Length', str(length))
            self.end_headers()
            self.wfile.write(body)
            self.close_connection = True

    http = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    worker = threading.Thread(target=http.serve_forever, daemon=True)
    worker.start()
    try:
        yield f'http://127.0.0.1:{http.server_port}', responses
    finally:
        http.shutdown()
        http.server_close()
        worker.join()


@pytest.mark.parametrize('changes', [
    {'application_id': 'another.app'}, {'schema': 'wrong'}, {'version': '0.1.0\n'},
    {'version': '../../bad'}, {'bytes': True}, {'bytes': '12'}, {'bytes': 1.5},
    {'bytes': 0}, {'bytes': 1073741825}, {'sha256': 'x' * 64},
    {'build_number': -1}, {'build_number': 0.5},
])
def test_invalid_release_is_rejected(changes):
    data = quote(json.dumps(manifest(**changes)))
    assert run(f'ConvertTo-LesRelease (ConvertFrom-Json {data})').returncode != 0


def test_manifest_uses_fixed_repository_and_ignores_supplied_executable_url(server):
    base, responses = server
    responses['/manifest'] = (json.dumps(manifest(installer_url='https://example.org/evil.exe')).encode(), 200, None)
    result = run(f'$c=New-LesHttpClient; try {{ (Get-LesRelease $c {quote(base + "/manifest")}).Uri.AbsoluteUri }} finally {{ $c.Dispose() }}')
    assert result.returncode == 0, result.stdout + result.stderr
    assert b'https://github.com/proovcme/LES/releases/download/v0.1.0/LES-RAG-Setup.exe' in result.stdout
    assert b'evil.exe' not in result.stdout


@pytest.mark.parametrize('case', ['valid', 'corrupt', 'truncated', 'oversized', 'http_error'])
def test_download_checks_actual_bytes_before_making_executable(tmp_path, server, case):
    base, responses = server
    body = PAYLOAD
    if case == 'corrupt':
        body = b'x' + body[1:]
    if case == 'truncated':
        body = body[:-30]
    if case == 'oversized':
        body += b'extra'
    # No Content-Length: catches actual streamed size, not only the header.
    responses['/payload'] = (body, 500 if case == 'http_error' else 200, None)
    target = tmp_path / "Лес # Café ' установщик"
    data = quote(json.dumps(manifest()))
    result = run(f'$r=ConvertTo-LesRelease (ConvertFrom-Json {data}); $r.Uri=[uri]{quote(base + "/payload")}; '
                 f'$c=New-LesHttpClient; try {{ Receive-LesInstaller $c $r {quote(target)} }} finally {{ $c.Dispose() }}')
    files = list(target.rglob('*.exe'))
    assert not list(target.rglob('*.part'))
    if case == 'valid':
        assert result.returncode == 0, result.stdout + result.stderr
        assert len(files) == 1 and files[0].read_bytes() == PAYLOAD
    else:
        assert result.returncode != 0
        assert not files


@pytest.mark.parametrize('body,status', [(b'not json', 200), (b'x' * 65537, 200), (b'', 404)],
                         ids=['invalid_json', 'oversized_manifest', 'unpublished_release'])
def test_bad_manifest_is_not_accepted(server, body, status):
    base, responses = server
    responses['/manifest'] = (body, status, None)
    result = run(f'$c=New-LesHttpClient; try {{ Get-LesRelease $c {quote(base + "/manifest")} }} finally {{ $c.Dispose() }}')
    assert result.returncode != 0
