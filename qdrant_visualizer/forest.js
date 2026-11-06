import {inventory,matches,documentUrl,chunkMatches,documentStatus,counted,documentSpecies,treeSpecies,fileSizeLabel} from './forest-model.js?v=800.2';
import {createForestScene} from './forest-scene.js?v=800.2';
const $=id=>document.getElementById(id);
const native=document.body.dataset.nativeForest==='true';
const api=native?'/api':'/lite-api',demo=document.body.dataset.demo==='true'||new URLSearchParams(location.search).get('demo')==='1';
let catalog={datasets:[],documents:[]},selected=null,query='',dataset='',limit=40,chunks=null,loadToken=0,selectionToken=0,searchToken=0;
function el(tag,text,className){const n=document.createElement(tag);if(text!==undefined)n.textContent=text;if(className)n.className=className;return n}
function speciesIcon(species){
  const icon=el('span',treeSpecies[species].icon,'tree-icon');
  icon.dataset.species=species;
  icon.setAttribute('aria-hidden','true');
  return icon;
}
for (const [key, species] of Object.entries(treeSpecies)) {
  const item=el('li');
  item.append(speciesIcon(key),el('span',`${species.name} · ${species.format}`));
  $('forest-species').append(item);
}
function say(message){$('status').textContent=message}
function clearSelection(){++selectionToken;selected=null;$('detail').hidden=true;$('detail').replaceChildren()}
async function request(path,options={}){const response=await fetch(api+path,{...options,signal:AbortSignal.timeout(30000)});if(!response.ok){const e=new Error('request');e.status=response.status;throw e}return response.json()}
function problem(error){return error.status===401||error.status===403?'Войдите в ЛЕС, чтобы открыть свои документы.':error.status===404?'Этот экран не подключён к каталогу ЛЕС. Откройте его из приложения.':'Не удалось получить данные. Проверьте состояние ЛЕС и повторите загрузку.'}
function filtered(){return catalog.documents.filter(d=>matches(d,chunks?'':query,dataset)&&(!chunks||chunks.some(c=>chunkMatches(d,c))))}
function render(){
  const docs=filtered(), focused=document.activeElement?.dataset?.documentId;
  $('documents').replaceChildren();
  for(const d of docs.slice(0,limit)){
    const b=el('button',undefined,'doc');b.type='button';b.dataset.documentId=d.id;
    b.setAttribute('aria-pressed',String(selected?.id===d.id));
    const icon=speciesIcon(documentSpecies(d));
    const text=el('span',String(d.label||'Без названия'),'name');
    text.append(el('small',`${d.chunks||0} фрагм. · ${documentStatus(d)}`));
    b.append(icon,text);b.onclick=()=>select(d);$('documents').append(b);
    if(focused===d.id)b.focus({preventScroll:true});
  }
  $('more').hidden=docs.length<=limit;
  $('count').textContent=dataset
    ? counted(docs.length,'документ','документа','документов')+' в роще'
    : `${counted(catalog.datasets.length,'роща','рощи','рощ')} · ${counted(catalog.documents.length,'документ','документа','документов')}`;
  scene.update(catalog,dataset,docs,selected?.id,Boolean(chunks||query.trim()));
}
async function select(doc){const token=++selectionToken;selected=doc;const panel=$('detail');panel.hidden=false;panel.replaceChildren(el('h2',doc.label),el('p',`${doc.dataset||'Датасет'} · ${counted(doc.chunks||0,'фрагмент','фрагмента','фрагментов')}`),el('p',documentStatus(doc)));const size=fileSizeLabel(doc.file_size);if(size)panel.append(el('p',size));for(const c of (chunks||[]).filter(c=>chunkMatches(doc,c))){panel.append(el('blockquote',c.content));if(c.rrf_rank)panel.append(el('p',`Место в объединённой выдаче RRF: ${c.rrf_rank}`))}render();if(demo){panel.append(el('p','Демонстрационный документ. Здесь нет пользовательских файлов.'));return}if(doc.document_id){const a=el('a','Открыть источник ↗');a.href=documentUrl({id:doc.document_id,dataset_id:doc.dataset_id,file_name:doc.label},api);a.target='_blank';a.rel='noopener';panel.append(a);return}const note=el('p','Находим оригинал…');panel.append(note);try{const data=await request('/rag/documents?'+new URLSearchParams({dataset_id:doc.dataset_id,q:doc.label,limit:'100'}));if(token!==selectionToken)return;const exact=(data.documents||[]).filter(d=>d.dataset_id===doc.dataset_id&&d.file_name===doc.label);note.remove();if(exact.length===1&&documentUrl(exact[0],api)){const a=el('a','Открыть источник ↗');a.href=documentUrl(exact[0],api);a.target='_blank';a.rel='noopener';panel.append(a)}else panel.append(el('p','Путь к оригиналу не найден однозначно. Проверьте файл в датасете.'))}catch(e){if(token===selectionToken)note.textContent=problem(e)}}
async function load(){const token=++loadToken;++selectionToken;++searchToken;$('reload').disabled=true;say('Загружаем каталог документов…');try{let data;if(demo){data={nodes:[]};for(const [i,name] of ['Проект дома','Исследования','Личная библиотека'].entries()){data.nodes.push({id:'ds:demo'+i,kind:'dataset',label:name});for(let n=0;n<24+i*12;n++)data.nodes.push({id:`demo${i}:${n}`,kind:'document',dataset_id:'demo'+i,dataset:name,label:`${['План','Заметки','Глава'][i]} ${n+1}.${['pdf','docx','xlsx','pptx','png','zip'][n%6]}`,file_size:2048*2**(n%16),chunks:5+(n*7)%120,status:'INDEXED'})}}else data=await request(native?'/light/forest':'/rag/graph/full');if(token!==loadToken)return;catalog=inventory(data);selected=null;chunks=null;$('detail').hidden=true;$('dataset').replaceChildren(new Option('Весь лес',''));for(const d of catalog.datasets)$('dataset').add(new Option(d.label,d.id.replace(/^ds:/,'')));if(!catalog.datasets.some(d=>d.id==='ds:'+dataset))dataset='';$('dataset').value=dataset;render();say(demo?'Демонстрация · вымышленные документы':catalog.documents.length?'Выберите документ или задайте вопрос.':'Лес пока пуст. Добавьте документы в датасет ЛЕС.')}catch(e){if(token===loadToken){say(problem(e));$('count').textContent=catalog.documents.length?'Показан предыдущий каталог':'Каталог недоступен'}}finally{if(token===loadToken)$('reload').disabled=false}}
function setDataset(value){
  clearSelection();dataset=value;$('dataset').value=value;chunks=null;++searchToken;limit=40;render();
  say(`Документов в выбранной области: ${filtered().length}`);
}
function overview(){
  if(dataset)setDataset('');
  else scene.reset();
}
$('reload').onclick=load;$('more').onclick=()=>{limit+=40;render()};$('dataset').onchange=()=>setDataset($('dataset').value);$('search').oninput=()=>{clearSelection();query=$('search').value;chunks=null;++searchToken;limit=40;render();say(`По названию найдено: ${filtered().length}`)};
$('search-form').onsubmit=async e=>{e.preventDefault();if(!query.trim())return;if(demo){say('В демонстрации доступен поиск по названию. Поиск по содержимому работает в ЛЕС.');return}const token=++searchToken;say('Ищем фрагменты в индексе…');try{const result=await request('/search',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({query,dataset_ids:dataset?[dataset]:undefined,top_k:20,max_chars:1600})});if(token!==searchToken)return;chunks=result.chunks||[];limit=40;render();const linked=chunks.filter(c=>catalog.documents.some(d=>chunkMatches(d,c))).length;say(`Найдено фрагментов: ${chunks.length}. Связано с деревьями: ${linked}.`);if(chunks.length&&!linked)say('Фрагменты найдены, но в ответе нет однозначной связи с каталогом. Откройте поиск в чате ЛЕС.')}catch(error){if(token===searchToken)say(problem(error))}};
const scene=createForestScene($('forest'),{
  minus:$('forest-minus'),plus:$('forest-plus'),scale:$('forest-scale'),overview:$('forest-overview'),
},select,setDataset,overview);
load();
