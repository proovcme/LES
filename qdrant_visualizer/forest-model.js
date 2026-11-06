// Stable inventory layout, deliberately not a semantic vector projection.
export function hash(value) { let h=2166136261; for(const c of String(value)){h^=c.codePointAt(0);h=Math.imul(h,16777619)} return h>>>0; }
export function inventory(payload) {
  if(!payload || !Array.isArray(payload.nodes)) throw new Error('invalid_catalog');
  return {datasets:payload.nodes.filter(n=>n.kind==='dataset'&&typeof n.id==='string'),documents:payload.nodes.filter(n=>n.kind==='document'&&typeof n.id==='string'&&typeof n.dataset_id==='string')};
}
export function matches(doc,query,dataset) { return (!dataset||doc.dataset_id===dataset)&&String(doc.label||'').toLocaleLowerCase('ru').includes(query.trim().toLocaleLowerCase('ru')); }
export function sourceUrl(path) { return '/lite-api/rag/file/viewer?'+new URLSearchParams({path:String(path)}); }
export function documentUrl(doc, api='/lite-api') { return doc?.id&&doc?.dataset_id&&doc?.file_name ? api+'/documents/by-id/'+encodeURIComponent(doc.id)+'/raw?'+new URLSearchParams({dataset_id:doc.dataset_id,doc_name:doc.file_name}) : ''; }
export function chunkMatches(doc,chunk) { const m=chunk.metadata||{}; return String(m.dataset_id||'')===doc.dataset_id&&String(chunk.doc_name||'')===doc.label; }
export const treeSpecies = {
  pdf: {name:'Ель', format:'PDF', icon:'▲', suffixes:['pdf']},
  text: {name:'Дуб', format:'Текст и Word', icon:'●', suffixes:['doc','docx','odt','rtf','txt','md','html','htm','epub','eml','msg']},
  table: {name:'Берёза', format:'Таблицы', icon:'⬮', suffixes:['xls','xlsx','xlsm','xlsb','ods','csv','tsv']},
  slides: {name:'Клён', format:'Презентации', icon:'◆', suffixes:['ppt','pptx','odp']},
  image: {name:'Кипарис', format:'Изображения', icon:'▴', suffixes:['png','jpg','jpeg','bmp','gif','tif','tiff','webp','svg','heic']},
  other: {name:'Можжевельник', format:'Другие файлы', icon:'⬟', suffixes:[]},
};

export function documentSpecies(doc) {
  const suffix = String(doc.file_name || doc.label || '').toLowerCase().match(/\.([^./\\]+)$/)?.[1];
  return Object.entries(treeSpecies).find(([,tree]) => tree.suffixes.includes(suffix))?.[0] ?? 'other';
}

export function fileSizeLabel(bytes) {
  if (!Number.isFinite(bytes) || bytes < 0) return '';
  const units = ['Б','КБ','МБ','ГБ','ТБ'];
  const unit = Math.min(Math.floor(Math.log2(Math.max(1, bytes)) / 10), units.length - 1);
  return `${(bytes / 1024 ** unit).toLocaleString('ru', {maximumFractionDigits:1})} ${units[unit]}`;
}

export function treePosition(doc,index,count) {
  const angle=index*2.399963229728653, radius=Math.sqrt((index+.5)/Math.max(count,1));
  const bytes = doc.file_size;
  const height = Number.isFinite(bytes) && bytes >= 0
    ? 15 + Math.max(0, Math.min(34, (Math.log2(Math.max(1,bytes)) - 10) * 1.85))
    : 23;
  return {x:Math.cos(angle)*radius, z:Math.sin(angle)*radius, height};
}

export function documentStatus(doc) { return doc.status === "INDEXED" && !Number(doc.chunks) ? "Текст для поиска отсутствует" : ({INDEXED:"Готов к поиску",PENDING:"Ожидает обработки",ERROR:"Нужна проверка",MISSING:"Файл не найден",SKIPPED:"Пропущен"}[doc.status] || "Статус не указан"); }

export function counted(n,one,few,many){const v=Math.abs(Number(n)),last=v%10,teen=v%100;return `${n} ${teen>=11&&teen<=14?many:last===1?one:last>=2&&last<=4?few:many}`}
