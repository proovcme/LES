"""Pure JS regressions for identity, literal search and safe source links."""
import json
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_light_forest_excludes_system_scope_not_user_names(tmp_path, monkeypatch):
    import sqlite3
    from backend import rag_config
    from proxy.services import graph_edges_service as graph, project_service
    db = tmp_path / "graph.db"
    with sqlite3.connect(db) as conn:
        conn.executescript("""
        CREATE TABLE datasets(id TEXT, name TEXT, chunk_count INT, group_name TEXT, dataset_scope TEXT);
        CREATE TABLE documents(id TEXT, dataset_id TEXT, file_name TEXT, doc_type TEXT, domain TEXT, chunk_count INT, status TEXT);
        INSERT INTO datasets VALUES('user','SMETA_SERVICE_Index',1,'','user'),('internal','Internal',1,'','system');
        INSERT INTO documents VALUES('doc1','user','mine.txt','','',1,'INDEXED'),('doc2','internal','service.txt','','',1,'INDEXED'),('temp','user','folder/~$owner.docx','','',0,'INDEXED');
        """)
    monkeypatch.setattr(rag_config, "rag_meta_db_path", lambda: str(db))
    monkeypatch.setattr(project_service, "build_registry", lambda: {"projects": []})
    monkeypatch.setattr(graph, "build_reference_edges", lambda *a, **kw: {"edges": []})
    light = graph.build_graph_full("test", include_system=False)
    assert {n['id'] for n in light['nodes']} == {'ds:user', 'user:mine.txt'}
    assert light['stats']['documents'] == 1
    assert next(n for n in light['nodes'] if n['kind'] == 'document')['document_id'] == 'doc1'
    full = graph.build_graph_full("test", include_system=True)
    assert full['stats']['datasets'] == 2
    assert full['stats']['documents'] == 2


def test_forest_actual_handlers_with_in_memory_dom():
    node = shutil.which("node")
    assert node, "Node is required for the forest contract gate"
    subprocess.run([node, "tests/forest_controls.mjs"], cwd=ROOT, check=True, capture_output=True, text=True)


def test_forest_identity_and_source_contract():
    node = shutil.which("node")
    assert node, "Node is required for the forest contract gate"
    module = (ROOT / "qdrant_visualizer/forest-model.js").read_text(encoding="utf-8")
    script = "const m=await import('data:text/javascript;base64,'+Buffer.from(" + json.dumps(module) + ").toString('base64'));\n" + r'''
    const assert = (await import('node:assert/strict')).default;
    const a={id:'a:same.pdf',dataset_id:'a',label:'same.pdf'};
    const b={id:'b:same.pdf',dataset_id:'b',label:'same.pdf'};
    const hit={doc_name:'same.pdf',metadata:{dataset_id:'a'}};
    assert.equal(m.chunkMatches(a,hit),true);
    assert.equal(m.chunkMatches(b,hit),false);
    assert.equal(m.chunkMatches(a,{doc_name:'same.pdf'}),false);
    const path='C:\\资料\\файл #?&<script>.pdf';
    const u=new URL(m.sourceUrl(path),'https://example.test');
    assert.equal(u.origin,'https://example.test');
    assert.equal(u.searchParams.get('path'),path);
    const uploaded={id:'doc:42/#?',dataset_id:'dataset & one',file_name:'файл #?&.txt',source_path:''};
    const original=new URL(m.documentUrl(uploaded),'https://example.test');
    assert.equal(original.pathname,'/lite-api/documents/by-id/doc%3A42%2F%23%3F/raw');
    assert.equal(original.searchParams.get('dataset_id'),uploaded.dataset_id);
    assert.equal(original.searchParams.get('doc_name'),uploaded.file_name);
    assert.equal(m.documentUrl({...uploaded,id:''}),'');
    assert.equal(m.matches({...a,label:'Ёлка [%]_🌲.pdf'},'[%]_🌲','a'),true);
    assert.equal(m.matches(a,'same','b'),false);
    assert.throws(()=>m.inventory({message:'unavailable'}));
    assert.deepEqual(m.inventory({nodes:[]}),{datasets:[],documents:[]});
    assert.equal(m.documentStatus({status:'INDEXED',chunks:0}),'Текст для поиска отсутствует');
    assert.equal(m.documentStatus({status:'PENDING',chunks:0}),'Ожидает обработки');
    assert.equal(m.documentStatus({status:'INDEXED',chunks:3}),'Готов к поиску');
    assert.deepEqual(m.treePosition(a,4,100),m.treePosition(a,4,100));
    for(let i=0;i<10000;i++){
      const p=m.treePosition({...a,id:String(i)},i,10000);
      assert.ok(Number.isFinite(p.x)&&Number.isFinite(p.z));
      assert.ok(p.x*p.x+p.z*p.z<=1.00001);
    }
    '''
    subprocess.run([node, "--input-type=module", "-e", script], check=True, capture_output=True, text=True)


def test_forest_is_same_origin_and_has_keyboard_alternative():
    script = (ROOT / "qdrant_visualizer/forest.js").read_text(encoding="utf-8")
    page = (ROOT / "qdrant_visualizer/index.html").read_text(encoding="utf-8-sig")
    assert "const api='/lite-api'" in script
    assert ":6333" not in script and ":8050" not in script
    assert ".innerHTML" not in script
    assert "requestAnimationFrame" in script and "setInterval" not in script
    assert "AbortSignal.timeout(30000)" in script
    assert 'id="documents"' in page and "aria-pressed" in script
    assert "token!==searchToken" in script and "token!==selectionToken" in script
