// Unit execution of real handlers with an in-memory DOM; no browser or visual claim.
import assert from 'node:assert/strict';
import fs from 'node:fs';

class Element {
  constructor(tag='div') { this.tag=tag; this.children=[]; this.style={}; this.dataset={}; this.value=''; this.hidden=false; }
  append(...items) { for(const item of items){item.parent=this;this.children.push(item)} }
  replaceChildren(...items) { this.children=[];this.append(...items) }
  setAttribute(name,value) { this[name]=value }
  add(item) { this.append(item) }
  remove() { if(this.parent)this.parent.children=this.parent.children.filter(item=>item!==this) }
  getContext() { return {} }
}
const ids=new Map();
const element=id=>{if(!ids.has(id))ids.set(id,new Element());return ids.get(id)};
globalThis.document={body:new Element('body'),getElementById:element,createElement:tag=>new Element(tag)};
globalThis.location={search:''};
globalThis.Option=class extends Element {constructor(text,value){super('option');this.textContent=text;this.value=value}};
globalThis.ResizeObserver=class {observe(){}};
globalThis.requestAnimationFrame=()=>1;

const docs=Array.from({length:85},(_,i)=>({id:'graph:'+i,kind:'document',dataset_id:i<80?'a':'b',label:`file${i}.txt`,document_id:i===1?'direct-id':null,status:'INDEXED',chunks:1}));
const catalog={nodes:[{id:'ds:a',kind:'dataset',label:'One'},{id:'ds:b',kind:'dataset',label:'Two'},...docs]};
let failure=0, searchResolve=null, delaySearch=false, requests=[];
globalThis.fetch=async (route,options)=>{
  requests.push({route,options});
  if(failure)return {ok:false,status:failure};
  if(route.endsWith('/rag/graph/full'))return {ok:true,json:async()=>catalog};
  if(route.includes('/rag/documents?')){
    const query=new URL(route,'http://fixture').searchParams;
    return {ok:true,json:async()=>({documents:[{id:'original:#1',dataset_id:query.get('dataset_id'),file_name:query.get('q'),source_path:''}]})};
  }
  if(route.endsWith('/search')){
    const result={ok:true,json:async()=>({chunks:[{doc_name:'file0.txt',metadata:{dataset_id:'a'},content:'Synthetic source',rrf_rank:1}]})};
    if(delaySearch)return new Promise(resolve=>{searchResolve=()=>resolve(result)});
    return result;
  }
  throw new Error('Unexpected route '+route);
};
const model='data:text/javascript;base64,'+Buffer.from(fs.readFileSync('qdrant_visualizer/forest-model.js')).toString('base64');
const code=fs.readFileSync('qdrant_visualizer/forest.js','utf8').replace("'./forest-model.js'",JSON.stringify(model));
await import('data:text/javascript;base64,'+Buffer.from(code).toString('base64'));
await new Promise(resolve=>setImmediate(resolve));
assert.equal(element('documents').children.length,40);
assert.equal(element('more').hidden,false);
element('more').onclick();
assert.equal(element('documents').children.length,80);
element('dataset').value='b';element('dataset').onchange();
assert.equal(element('documents').children.length,5);
element('dataset').value='a';element('dataset').onchange();
element('search').value='file0';element('search').oninput();
assert.equal(element('documents').children.length,1);
await element('documents').children[0].onclick();
const original=element('detail').children.find(item=>item.tag==='a');
assert.ok(original,'Uploaded file without external source_path must be openable');
assert.ok(original.href.startsWith('/lite-api/documents/by-id/original%3A%231/raw?'));
assert.equal(new URL(original.href,'http://fixture').searchParams.get('dataset_id'),'a');
element('search').value='file1.txt';element('search').oninput();
const beforeDirect=requests.length;
await element('documents').children[0].onclick();
assert.equal(requests.length,beforeDirect,'Exact ID must avoid paginated name search');
assert.ok(element('detail').children.find(item=>item.tag==='a').href.includes('/direct-id/raw?'));
element('search').value='file0';element('search').oninput();
element('dataset').value='b';element('dataset').onchange();
assert.equal(element('detail').hidden,true,'Changing dataset must dismiss the previous source');
element('dataset').value='a';element('dataset').onchange();

await element('search-form').onsubmit({preventDefault(){}});
assert.equal(element('documents').children.length,1);
const submitted=JSON.parse(requests.findLast(item=>item.route.endsWith('/search')).options.body);
assert.deepEqual(submitted.dataset_ids,['a']);
assert.equal(submitted.query,'file0');
delaySearch=true;
const pending=element('search-form').onsubmit({preventDefault(){}});
element('search').value='file79';element('search').oninput();
searchResolve();await pending;
assert.equal(element('documents').children.length,1);
assert.equal(element('documents').children[0].children[1].textContent,'file79.txt');

failure=503;await element('reload').onclick();
assert.equal(element('reload').disabled,false);
assert.equal(element('documents').children.length,1,'Failed reload retains previous catalog');
assert.ok(element('status').textContent.includes('Не удалось получить данные'));
failure=403;await element('reload').onclick();
assert.ok(element('status').textContent.includes('Войдите в ЛЕС'));
failure=0;await element('reload').onclick();
assert.equal(element('reload').disabled,false);
console.log('forest handler unit scenarios passed');
