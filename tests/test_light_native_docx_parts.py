"""Word payload preservation, bounded large XML and revision-pinned media downloads."""
import hashlib
import json
import os
import sqlite3
import subprocess
import io
import zipfile
from urllib.parse import unquote

import httpx
import pytest
from backend import native_core
from tests.test_light_native_docx_semantics import convert, term
from tests.test_light_native_api import gateway  # noqa: F401
from tests.test_light_native_chat_turn import native_turn, PATH, HEADERS  # noqa: F401


@pytest.mark.skipif(not os.getenv('LES_CORE_LIBRARY'),reason='Native core required')
def test_large_formatting_xml_keeps_tail_and_invalid_utf8_fails_closed(tmp_path):
    formatting='<w:r><w:rPr>'+('<w:lang w:val="ru-RU"/>'*110)+'</w:rPr><w:t>Строка 東京</w:t></w:r>'
    value=convert(tmp_path,'<w:p>'+formatting*4100+'<w:r><w:t>LAST 0,125 м²</w:t></w:r></w:p>')
    assert value['text'].count('Строка 東京')==4100 and value['text'].endswith('LAST 0,125 м²')
    path=tmp_path/'invalid.docx'
    with zipfile.ZipFile(path,'w') as archive:
        archive.writestr('word/document.xml',b'<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>\xff</w:t></w:r></w:p></w:body></w:document>')
    with pytest.raises(native_core.NativeCoreError,match='^DOCUMENT_XML_INVALID$'):
        native_core.required('document',request={'schema':'les.document-convert.v1','action':'convert','path':str(path.resolve())})


@pytest.mark.skipif(not os.getenv('LES_CORE_LIBRARY'),reason='Native core required')
def test_nested_table_column_break_and_formatted_math_preserve_boundaries(tmp_path):
    inner='<w:tbl><w:tr><w:tc><w:p><w:r><w:t>INNER 42</w:t></w:r></w:p></w:tc></w:tr></w:tbl>'
    table='<w:tbl><w:tr><w:tc><w:p><w:r><w:t>BEFORE</w:t></w:r></w:p>'+inner+'<w:p><w:r><w:t>AFTER</w:t></w:r></w:p></w:tc></w:tr></w:tbl>'
    function='<m:func><m:funcPr><m:ctrlPr><w:rPr><w:i/></w:rPr></m:ctrlPr></m:funcPr><m:fName>'+term('cos')+'</m:fName><m:e>'+term('φ')+'</m:e></m:func>'
    root='<m:rad><m:radPr><m:degHide m:val="1"/></m:radPr><m:deg/><m:e>'+function+'</m:e></m:rad>'
    fraction='<m:f><m:fPr><m:ctrlPr><w:rPr><w:i/></w:rPr></m:ctrlPr></m:fPr><m:num>'+root+'</m:num><m:den>'+term('25')+'</m:den></m:f>'
    result=convert(tmp_path,table+'<w:p><w:r><w:t>A</w:t><w:br w:type="column"/><w:t>B</w:t></w:r><m:oMath>'+fraction+'</m:oMath></w:p>')['text']
    assert result.index('BEFORE')<result.index('INNER 42')<result.index('AFTER')
    assert 'A\nB(sqrt(cos(φ)))/(25)' in result
    spaced=convert(tmp_path,'<w:p><m:oMath>'+term('a')+term(' ')+term('b')+'</m:oMath></w:p>')['text']
    assert spaced=='a b'


@pytest.mark.parametrize('mismatch', [False, True])
def test_exact_word_member_cannot_read_neighbor_ocr_or_hide_identity_failure(native_turn, mismatch):
    """Controlled cached parts test identity/receipts, not EMF rendering or OCR accuracy."""
    t=native_turn
    path=t.state/'Selected.docx'
    with zipfile.ZipFile(path,'w') as z:
        z.writestr('word/document.xml','<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body/></w:document>')
    source=path.read_bytes();sha=hashlib.sha256(source).hexdigest()
    left=b'cached neighbor bytes';right=b'cached requested bytes'
    left_sha=hashlib.sha256(left).hexdigest();right_sha=hashlib.sha256(right).hexdigest()
    root=t.state/'storage/docx-parts'/sha;root.mkdir(parents=True)
    for key,data in ((sha,source),(left_sha,left),(right_sha,right)):(root/key).write_bytes(data)
    manifest={'schema':'les.docx-parts.v1','source_sha256':sha,'parts':[
        {'kind':'image','member':'word/media/image6.emf','sha256':left_sha,'text':'NEIGHBOR OCR MUST NOT LEAK'},
        {'kind':'image','member':'word/media/image7.emf','sha256':right_sha,'text':'','coverage':'unread_visual'}]}
    raw=json.dumps(manifest).encode();msha=hashlib.sha256(raw).hexdigest()
    (root/msha).write_bytes(raw);(root/'manifest.json').write_text(json.dumps({'sha256':msha}))
    with sqlite3.connect(t.database) as db:
        db.execute('CREATE TABLE datasets(id TEXT PRIMARY KEY,name TEXT,status TEXT,chunk_count INTEGER,dataset_scope TEXT,module_id TEXT)')
        db.execute("INSERT INTO datasets VALUES('ds','Fixture','INDEXED',1,'user','')")
        db.execute('CREATE TABLE documents(id TEXT,dataset_id TEXT,file_name TEXT,source_path TEXT,file_hash TEXT,status TEXT)')
        db.execute('INSERT INTO documents VALUES(?,?,?,?,?,?)',('selected','ds','Selected.docx',str(path),sha,'INDEXED'))
    args={'doc_id':'selected','view':'part','member':'word/media/image7.emf','source_sha256':sha}
    if mismatch:args['part_id']=left_sha
    t.scripts.extend([{'tool':'read_source','args':args},{'text':'Содержимое рисунка не прочитано.'}])
    response=t.client.post(PATH,json={'question':'Прочитай выбранную часть','dataset_ids':['ds'],'target_files':['Selected.docx']},headers=HEADERS)
    if mismatch:
        assert response.status_code==503 and response.json()['code']=='SOURCE_PART_IDENTITY_MISMATCH'
        assert len(t.seen)==1 and len(t.scripts)==1
        with sqlite3.connect(t.database) as db:
            success,artifact=db.execute('SELECT success,artifact_json FROM chat_history ORDER BY id DESC LIMIT 1').fetchone()
        assert not success and json.loads(artifact)['error']['code']=='SOURCE_PART_IDENTITY_MISMATCH'
    else:
        result=response.raise_for_status().json();part=result['artifact']['tool_results'][0]['result']
        assert part['part']['member']=='word/media/image7.emf' and part['part_sha256']==right_sha
        assert part['text']=='' and part['evidence_admissible'] is False
        assert 'NEIGHBOR OCR MUST NOT LEAK' not in json.dumps(t.seen,ensure_ascii=False)
        assert result['source_map']==[]
    assert hashlib.sha256(path.read_bytes()).hexdigest()==sha


def test_parts_are_authorized_hashed_ranged_and_pinned_to_catalog_revision(gateway):
    client,database,folder=gateway
    source=b'original Word ZIP';sha=hashlib.sha256(source).hexdigest()
    image=b'\x89PNG\r\n\x1a\n' + b'fixture pixels';part=hashlib.sha256(image).hexdigest()
    root=folder/'state/storage/docx-parts'/sha;root.mkdir(parents=True)
    (root/sha).write_bytes(source);(root/part).write_bytes(image)
    manifest={'schema':'les.docx-parts.v1','source_sha256':sha,'parts':[{'kind':'image','member':'word/media/picture.wmf','sha256':part,'preview_sha256':part,'bytes':len(image),'text':'OCR is not a geometry proof'}]}
    raw=json.dumps(manifest).encode();msha=hashlib.sha256(raw).hexdigest()
    (root/msha).write_bytes(raw);(root/'manifest.json').write_text(json.dumps({'sha256':msha}))
    with sqlite3.connect(database) as db:
        db.executescript('CREATE TABLE datasets(id TEXT PRIMARY KEY,name TEXT,status TEXT,dataset_scope TEXT,module_id TEXT);CREATE TABLE documents(id TEXT PRIMARY KEY,dataset_id TEXT,file_name TEXT,status TEXT,file_hash TEXT);')
        db.execute("INSERT INTO datasets VALUES('ds','fixture','IDLE','user','')")
        db.execute("INSERT INTO documents VALUES('doc','ds','Документ 東京.docx','INDEXED',?)",[sha])
    url='/api/documents/by-id/doc/parts'
    with httpx.Client(base_url=client.base_url,trust_env=False) as anonymous:assert anonymous.get(url).status_code==401
    listing=client.get(url).raise_for_status().json();assert listing['parts'][0]['label']=='Рисунок 1'
    image_url=listing['parts'][0]['preview_url'];response=client.get(image_url)
    assert response.content==image and response.headers['x-les-handler']=='rust.document_parts'
    assert response.headers['content-type']=='image/png' and '.png' in unquote(response.headers['content-disposition'])
    ranged=client.get(image_url,headers={'Range':'bytes=2-8'});assert ranged.status_code==206 and ranged.content==image[2:9]
    assert client.get(image_url,headers={'Range':'bytes=2-8','If-Range':'unknown'}).content==image
    head=client.head(image_url);assert head.status_code==200 and not head.content and int(head.headers['content-length'])==len(image)
    assert client.get(url+'/'+('a'*64)).status_code==404
    assert client.get(listing['source_url']).content==source
    (root/part).write_bytes(b'changed image');assert client.get(image_url).json()['code']=='SOURCE_PART_CORRUPT'
    with sqlite3.connect(database) as db:db.execute("UPDATE documents SET file_hash=? WHERE id='doc'",['b'*64])
    assert client.get(image_url).status_code==409


@pytest.mark.skipif(os.name!='nt',reason='Windows OCR process adapter')
def test_ocr_cache_ignores_relationship_ids_but_preserves_crop_and_each_revision(tmp_path):
    """Controlled OCR output tests caching/ownership, not recognition accuracy."""
    from PIL import Image
    api=os.getenv('LES_TEST_NATIVE_API')
    if not api:pytest.skip('Native API required')
    buffer=io.BytesIO();Image.new('RGB',(16,16),'red').save(buffer,format='PNG');png=buffer.getvalue()
    helper=tmp_path/'controlled-ocr.ps1'
    helper.write_text('param([string]$ImagePath)\n[IO.File]::AppendAllText((Join-Path $PSScriptRoot "ocr-calls"),"x")\nWrite-Output \'{"text":"CONTROLLED OCR"}\'\n',encoding='utf-8')
    scratch=tmp_path/'scratch';scratch.mkdir();frames=[];sources=[]
    for i in range(3):
        path=tmp_path/f'source-{i}.docx';state=tmp_path/f'state-{i}';state.mkdir()
        rid=f'picture-{i}';crop=' cropright="16384f"' if i==2 else ''
        with zipfile.ZipFile(path,'w') as z:
            z.writestr('word/document.xml',f'<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:v="v" xmlns:r="r"><w:body><w:p><w:r><w:t>Source {i}</w:t><w:pict><v:shape><v:imagedata r:id="{rid}"{crop}/></v:shape></w:pict></w:r></w:p></w:body></w:document>')
            z.writestr('word/_rels/document.xml.rels',f'<Relationships><Relationship Id="{rid}" Type="x/image" Target="media/shared.png"/></Relationships>')
            z.writestr('word/media/shared.png',png)
        frames.append({'request_id':i,'input':{'schema':'les.document-convert.v1','path':str(path.resolve()),'owned_state':str(state.resolve()),
            'ocr':{'engine':'windows','helper':str(helper.resolve()),'scratch':str(scratch.resolve()),'dpi':150}}})
        sources.append((path,state))
    result=subprocess.run([api,'--serve-document-formats'],input=''.join(json.dumps(f)+'\n' for f in frames),
        text=True,encoding='utf-8',capture_output=True,timeout=25,
        env={**os.environ,'PATH':'','PYTHONHOME':str(tmp_path/'missing'),'PYTHONPATH':str(tmp_path/'missing')})
    assert result.returncode==0
    rows=[json.loads(line)['result'] for line in result.stdout.splitlines()]
    assert len(rows)==3 and all(r['status']=='completed' for r in rows)
    assert (tmp_path/'ocr-calls').read_text()=='xx'
    previews=[]
    for path,state in sources:
        sha=hashlib.sha256(path.read_bytes()).hexdigest();root=state/'storage/docx-parts'/sha
        pointer=json.loads((root/'manifest.json').read_text());manifest=json.loads((root/pointer['sha256']).read_bytes())
        image=next(p for p in manifest['parts'] if p['kind']=='image');preview=image['preview_sha256']
        assert hashlib.sha256((root/preview).read_bytes()).hexdigest()==preview
        assert (root/image['sha256']).read_bytes()==png and (root/sha).read_bytes()==path.read_bytes()
        previews.append(preview)
    assert previews[0]==previews[1] and previews[2]!=previews[0]
    assert not list(scratch.iterdir())
