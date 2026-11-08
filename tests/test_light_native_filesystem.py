"""Real read-only paths, Unicode windows, bounded traversal and source hashes."""
import hashlib
import os
from pathlib import Path

import pytest
from backend import native_core


@pytest.fixture
def files(tmp_path):
    root=tmp_path/'Разрешённая папка 日本語'
    root.mkdir()
    def run(action, **args):
        return native_core.required('filesystem',request={'schema':'les.filesystem.v1',
            'roots':{'docs':str(root)},'action':action,'arguments':args})
    return root,run


def test_read_hash_search_unicode_and_python_harness_use_the_same_native_owner(files,monkeypatch):
    root,run=files
    text='Начало строки. Straße 日本語. Конец строки.'
    path=root/'Файл Straße с пробелами.txt';path.write_text(text,encoding='utf-8')
    before=path.read_bytes();stamp=path.stat().st_mtime_ns
    result=run('read_text',path=path.name)
    assert result['status']=='completed',result
    value=result['value']['result']
    assert value['text']==text and not value['text_truncated']
    assert value['source_sha256']==hashlib.sha256(before).hexdigest()
    hashed=run('hash',path=path.name)
    assert hashed['value']['result']['sha256']==value['source_sha256']
    found=run('search',q='STRASSE',content=True)
    assert found['value']['result']['count']==1
    assert 'Straße 日本語' in found['value']['result']['hits'][0]['snippet']
    assert found['value']['result']['scan_truncated'] is False
    from proxy.services import tool_harness_service as harness
    monkeypatch.setattr(harness,'_allowed_roots',lambda:{'docs':root})
    actual=harness._tool_filesystem_read_text({'path':path.name})
    assert actual['status']=='ok' and actual['result']['text']==text
    assert actual['sources'][0]['path']==path.name
    assert actual['contract_check']['ok'] is True
    assert path.read_bytes()==before and path.stat().st_mtime_ns==stamp


@pytest.mark.parametrize('path',['../outside.txt','sub/../../outside.txt','.env','sub/.git/config','file.txt:stream',r'C:\Windows\system.ini'])
def test_model_paths_cannot_escape_roots_or_read_private_segments(files,path):
    _,run=files
    value=run('read_text',path=path)
    assert value['status']=='failed' and value['code']=='FILESYSTEM_SCOPE_DENIED',value


def test_limits_missing_and_invalid_utf8_are_explicit(files):
    root,run=files
    (root/'broken.txt').write_bytes(b'prefix \xff suffix')
    value=run('read_text',path='broken.txt')['value']
    assert value['status']=='ok' and '\ufffd' in value['result']['text'] and value['warnings']
    (root/'large.txt').write_bytes(b'a'*1_000_001)
    assert run('read_text',path='large.txt')['code']=='FILESYSTEM_TEXT_LIMIT'
    assert run('stat',path='missing.txt')['value']['status']=='missing'
    assert run('read_text',root='unknown',path='broken.txt')['code']=='FILESYSTEM_SCOPE_DENIED'


def test_file_tool_failure_keeps_reason_and_stops_answer_generation(files,monkeypatch):
    root,_=files
    from proxy.services import tool_harness_service as harness
    from proxy.services.pdf_source_evidence_service import require_source_read_success
    monkeypatch.setattr(harness,'_allowed_roots',lambda:{'docs':root})
    failed=harness._tool_filesystem_read_text({'path':'../outside.txt'})
    assert failed['status']=='error'
    assert failed['result']['code']=='FILESYSTEM_SCOPE_DENIED'
    with pytest.raises(Exception) as error:
        require_source_read_success(failed)
    assert error.value.detail['code']=='FILESYSTEM_SCOPE_DENIED'


def test_recursive_listing_and_search_never_follow_a_junction_outside_the_allowed_root(files,tmp_path):
    root,run=files
    outside=tmp_path/'Outside';outside.mkdir()
    (outside/'confidential.txt').write_text('must not enter tool output',encoding='utf-8')
    target=root/'shortcut'
    if os.name=='nt':
        import _winapi
        _winapi.CreateJunction(str(outside),str(target))
    else:
        target.symlink_to(outside,target_is_directory=True)
    listed=run('list',depth=4)
    assert listed['status']=='completed'
    assert listed['value']['result']['children']==[]
    assert listed['value']['result']['excluded_entries']==1
    assert run('search',q='confidential',content=True)['value']['result']['hits']==[]
    assert run('read_text',path='shortcut/confidential.txt')['code']=='FILESYSTEM_SCOPE_DENIED'
