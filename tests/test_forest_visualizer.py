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
    document = next(n for n in light['nodes'] if n['kind'] == 'document')
    assert document['document_id'] == 'doc1'
    assert document['file_size'] is None
    with sqlite3.connect(db) as conn:
        conn.execute("ALTER TABLE documents ADD COLUMN file_size INTEGER")
        conn.execute("UPDATE documents SET file_size = 8192 WHERE id = 'doc1'")
    full = graph.build_graph_full("test", include_system=True)
    assert full['stats']['datasets'] == 2
    assert full['stats']['documents'] == 2
    assert next(n for n in full['nodes'] if n['id'] == 'user:mine.txt')['file_size'] == 8192


def test_forest_actual_handlers_with_in_memory_dom():
    node = shutil.which("node")
    assert node, "Node is required for the forest contract gate"
    subprocess.run([node, "tests/forest_controls.mjs"], cwd=ROOT, check=True, capture_output=True, text=True)


def test_search_lab_links_exact_workbook_row():
    script = r"""
      import assert from 'node:assert/strict';
      import {readFileSync} from 'node:fs';
      class Element {
        constructor(tag) { this.tag = tag; this.children = []; }
        append(...nodes) { this.children.push(...nodes); }
        addEventListener(type, action) { this[type] = action; }
        setAttribute() {}
        replaceChildren() { this.children = []; }
      }
      const fields = new Map();
      globalThis.document = {
        body: {dataset:{nativeForest:'true'}}, createElement:(tag)=>new Element(tag),
        getElementById(id) {
          if (!fields.has(id)) fields.set(id, new Element(id));
          return fields.get(id);
        },
      };
      const hash = 'a'.repeat(64);
      globalThis.fetch = async (url)=>({ok:true, json:async ()=> url.endsWith('/datasets') ? [] : {
        chunks:[{doc_id:'id',doc_name:'Таблица.xlsx',content:'Сумма 12',
          metadata:{dataset_id:'test',source_sha256:hash,sheet_name:'Лист №1',row:12}}],
      }});
      const source = readFileSync('qdrant_visualizer/search.js', 'utf8');
      await import('data:text/javascript;base64,'+Buffer.from(source).toString('base64'));
      document.getElementById('lab-question').value='строка 12';
      await fields.get('lab-form').submit({preventDefault(){}});
      const [,list] = fields.get('lab-results').children;
      const anchor = list.children[0].children.find(node=>node.tag==='a');
      const original = new URL(anchor.href, 'http://localhost');
      assert.equal(original.pathname, '/api/documents/by-id/id/viewer');
      assert.equal(original.searchParams.get('locator'), 'Лист №1!R12');
      assert.equal(original.searchParams.get('source_sha256'), hash);
    """
    run = subprocess.run([shutil.which("node"), "--input-type=module", "-e", script], cwd=ROOT,
                         capture_output=True, encoding="utf-8")
    assert run.returncode == 0, run.stderr[-2000:]


def test_forest_identity_and_source_contract():
    node = shutil.which("node")
    assert node, "Node is required for the forest contract gate"
    module = (ROOT / "qdrant_visualizer/forest-model.js").read_text(encoding="utf-8")
    script = "const m=await import('data:text/javascript;base64,'+Buffer.from(" + json.dumps(module) + ").toString('base64'));\n" + r'''
    const assert = (await import('node:assert/strict')).default;
    for(const [n,w] of [[0,'рощ'],[1,'роща'],[2,'рощи'],[5,'рощ'],[11,'рощ'],[12,'рощ'],[14,'рощ'],[21,'роща'],[22,'рощи'],[101,'роща'],[111,'рощ']])
      assert.equal(m.counted(n,'роща','рощи','рощ'),`${n} ${w}`);
    assert.equal(m.counted(2,'документ','документа','документов'),'2 документа');

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
    assert.equal(m.documentSpecies({label:'ОВ / Чертёж.PDF'}),'pdf');
    assert.equal(m.documentSpecies({label:'Исходные данные.docx'}),'text');
    assert.equal(m.documentSpecies({label:'Ведомость.XLSX'}),'table');
    assert.equal(m.documentSpecies({label:'Слайды.pptx'}),'slides');
    assert.equal(m.documentSpecies({label:'Скан.TIFF'}),'image');
    assert.equal(m.documentSpecies({label:'папка.pdf/без формата'}),'other');
    assert.equal(m.fileSizeLabel(1024),'1 КБ');
    assert.equal(m.fileSizeLabel(null),'');
    assert.ok(m.treePosition({...a,file_size:50_000},4,100).height
      < m.treePosition({...a,file_size:50_000_000},4,100).height);
    for(let i=0;i<10000;i++){
      const p=m.treePosition({...a,id:String(i)},i,10000);
      assert.ok(Number.isFinite(p.x)&&Number.isFinite(p.z));
      assert.ok(p.x*p.x+p.z*p.z<=1.00001);
    }
    '''
    subprocess.run([node, "--input-type=module", "-e", script], check=True, capture_output=True, text=True)


def test_forest_is_same_origin_and_has_keyboard_alternative():
    script = (ROOT / "qdrant_visualizer/forest.js").read_text(encoding="utf-8")
    scene = (ROOT / "qdrant_visualizer/forest-scene.js").read_text(encoding="utf-8")
    page = (ROOT / "qdrant_visualizer/index.html").read_text(encoding="utf-8-sig")
    assert "const api=native?'/api':'/lite-api'" in script
    assert ":6333" not in script and ":8050" not in script
    assert ".innerHTML" not in script
    assert "requestAnimationFrame" in scene and "setInterval" not in scene
    assert 'id="forest" tabindex="0"' in page and 'aria-describedby="forest-instructions"' in page
    assert "AbortSignal.timeout(30000)" in script
    assert 'id="documents"' in page and "aria-pressed" in script
    assert "token!==searchToken" in script and "token!==selectionToken" in script
