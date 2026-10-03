// Stable inventory layout, deliberately not a semantic vector projection.
export function hash(value) { let h=2166136261; for(const c of String(value)){h^=c.codePointAt(0);h=Math.imul(h,16777619)} return h>>>0; }
export function inventory(payload) {
  if(!payload || !Array.isArray(payload.nodes)) throw new Error('invalid_catalog');
  return {datasets:payload.nodes.filter(n=>n.kind==='dataset'&&typeof n.id==='string'),documents:payload.nodes.filter(n=>n.kind==='document'&&typeof n.id==='string'&&typeof n.dataset_id==='string')};
}
export function matches(doc,query,dataset) { return (!dataset||doc.dataset_id===dataset)&&String(doc.label||'').toLocaleLowerCase('ru').includes(query.trim().toLocaleLowerCase('ru')); }
export function sourceUrl(path) { return '/lite-api/rag/file/viewer?'+new URLSearchParams({path:String(path)}); }
export function documentUrl(doc) { return doc?.id&&doc?.dataset_id&&doc?.file_name ? '/lite-api/documents/by-id/'+encodeURIComponent(doc.id)+'/raw?'+new URLSearchParams({dataset_id:doc.dataset_id,doc_name:doc.file_name}) : ''; }
export function chunkMatches(doc,chunk) { const m=chunk.metadata||{}; return String(m.dataset_id||'')===doc.dataset_id&&String(chunk.doc_name||'')===doc.label; }
export function treePosition(doc,index,count) { const angle=index*2.399963229728653,radius=Math.sqrt((index+.5)/Math.max(count,1)); return {x:Math.cos(angle)*radius,z:Math.sin(angle)*radius,height:24+hash(doc.id)%20}; }

export function documentStatus(doc) { return doc.status === "INDEXED" && !Number(doc.chunks) ? "Текст для поиска отсутствует" : ({INDEXED:"Готов к поиску",PENDING:"Ожидает обработки",ERROR:"Нужна проверка",MISSING:"Файл не найден",SKIPPED:"Пропущен"}[doc.status] || "Статус не указан"); }
