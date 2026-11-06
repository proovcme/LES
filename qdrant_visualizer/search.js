const native = document.body.dataset.nativeForest === 'true';
const api = native ? '/api' : '/lite-api';
const byId = (id) => document.getElementById(id);
const form = byId('lab-form');
const dataset = byId('lab-dataset');
const results = byId('lab-results');
const status = byId('lab-status');
if (native) byId('lab-chat').href = '/app/';

function element(tag, text, className) {
  const node = document.createElement(tag);
  node.textContent = text;
  if (className) node.className = className;
  return node;
}

async function request(path, options) {
  const response = await fetch(api + path, { signal: AbortSignal.timeout(60000), ...options });
  if (!response.ok) {
    const error = await response.json().catch(() => ({}));
    throw new Error(typeof error.detail === 'string' ? error.detail : 'Сервис поиска недоступен. Проверьте состояние Леса.');
  }
  return response.json();
}

function appendHit(list, hit) {
  const meta = hit.metadata ?? {};
  const location = [
    meta.section_heading,
    meta.sheet || meta.sheet_name,
    meta.row || meta.row_number ? `строка ${meta.row || meta.row_number}` : '',
    meta.page || meta.source_page ? `стр. ${meta.page || meta.source_page}` : '',
  ].filter(Boolean).join(' · ');
  const row = element('li', '', 'lab-hit');
  const title = element('h3', hit.doc_name || 'Документ');
  const detail = element('details', '');
  detail.append(element('summary', 'Почему найден и что дочитано'));
  const channels = hit.retrieval_sources;
  const channelNames = { lexical: 'По словам', sparse: 'По словам', vector: 'По смыслу', dense: 'По смыслу' };
  const ranking = [
    Array.isArray(channels) ? [...new Set(channels.map((channel) => channelNames[channel] || channel))].join(', ') : '',
    hit.rrf_rank ? `RRF · место ${hit.rrf_rank}` : '',
    hit.rrf_score != null ? `оценка RRF ${hit.rrf_score}` : '',
  ].filter(Boolean);
  detail.append(element('p', ranking.join(' · ') || 'Ранги отдельных каналов недоступны.'));
  detail.append(element('blockquote', hit.context?.content || 'Дополнительный контекст недоступен.'));
  if (hit.context?.metadata?.context_omitted_fragments) {
    detail.append(element('p', 'Часть фрагментов раздела не вошла в окно просмотра.'));
  }
  row.append(title, element('p', location, 'lab-location'), element('blockquote', hit.content || 'Фрагмент без текста.'));
  if (hit.doc_id && hit.doc_name && meta.dataset_id) {
    const link = element('a', meta.source_sha256 ? 'Открыть оригинал этой цитаты ↗' : 'Открыть текущий оригинал ↗');
    const query = new URLSearchParams({ dataset_id: meta.dataset_id, doc_name: hit.doc_name });
    if (meta.source_sha256) query.set('source_sha256', meta.source_sha256);
    const page = Number(meta.page || meta.source_page);
    const sheet = meta.sheet || meta.sheet_name;
    const sheetRow = meta.row || meta.row_number;
    const table = /\.xls[xm]$/i.test(hit.doc_name) && sheet && sheetRow;
    if (table) {
      query.set('sheet', sheet);
      query.set('locator', `${sheet}!R${sheetRow}`);
    }
    link.href = `${api}/documents/by-id/${encodeURIComponent(hit.doc_id)}/${table ? 'viewer' : 'raw'}?${query}`;
    if (/\.pdf$/i.test(hit.doc_name) && Number.isInteger(page) && page > 0) link.href += `#page=${page}`;
    link.target = '_blank';
    link.rel = 'noopener';
    row.append(link);
  }
  row.append(detail);
  const number = String(meta.section_heading || meta.parent_heading || '')
    .match(/^\s*#*\s*(?:пункт\s+)?(\d{1,5}(?:\.\d{1,5}){1,5})(?:\.?(?:\s|$))/iu)?.[1];
  if (number && hit.doc_id && meta.dataset_id) {
    const button = element('button', `Читать пункт ${number}`, 'quiet');
    const output = element('div', '');
    output.setAttribute('aria-live', 'polite');
    button.addEventListener('click', async () => {
      button.disabled = true;
      output.textContent = 'Читаем пункт…';
      try {
        const args = new URLSearchParams({ number, dataset_id: meta.dataset_id });
        const data = await request(`/documents/by-id/${encodeURIComponent(hit.doc_id)}/clause?${args}`);
        output.textContent = data.status === 'found' ? 'Фрагменты пункта в индексе'
          : data.status === 'ambiguous' ? 'В файле несколько совпадающих заголовков. Проверьте оригинал.'
          : 'Пункт с таким заголовком отсутствует в индексе.';
        for (const source of data.sources || []) {
          const page = source.page || source.source_page || '';
          if (page) output.append(element('p', `Страница ${page}`, 'lab-location'));
          output.append(element('blockquote', source.snippet || ''));
        }
      } catch (error) {
        output.textContent = error.message;
      } finally {
        button.disabled = false;
      }
    });
    row.append(button, output);
  }
  list.append(row);
}

form.addEventListener('submit', async (event) => {
  event.preventDefault();
  const query = byId('lab-question').value.trim();
  if (!query) { status.textContent = 'Введите вопрос для поиска.'; return; }
  const submit = byId('lab-submit');
  submit.disabled = true;
  results.replaceChildren();
  results.setAttribute('aria-busy', 'true');
  status.textContent = 'Ищем и дочитываем фрагменты…';
  try {
    const started = performance.now();
    const data = await request('/search', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        query, dataset_ids: dataset.value ? [dataset.value] : undefined,
        top_k: 10, include_trace: true, include_context: true,
        reranker_enabled: byId('lab-rerank').checked,
      }),
    });
    const trace = data.retrieval_trace ?? {};
    const hits = data.chunks ?? [];
    if (!hits.length) {
      status.textContent = trace.error_code
        ? 'Поиск не завершился. Проверьте доступность индекса и поисковой модели.'
        : 'Подходящие фрагменты не найдены.';
      return;
    }
    status.textContent = `Найдено фрагментов: ${hits.length} · ${Math.round(performance.now() - started)} мс`;
    const summary = element('p', [
      trace.vector_count ? `По смыслу: ${trace.vector_count}` : '',
      trace.lexical_count ? `По словам: ${trace.lexical_count}` : '',
      trace.fusion === 'rrf' ? 'Объединение RRF' : '',
    ].filter(Boolean).join(' · '), 'lab-location');
    const list = element('ol', '', 'lab-hits');
    hits.forEach((hit) => appendHit(list, hit));
    results.append(summary, list);
  } catch (error) {
    status.textContent = error.name === 'TimeoutError' ? 'Поиск занял больше минуты. Попробуйте выбрать одну рощу.' : error.message;
  } finally {
    submit.disabled = false;
    results.setAttribute('aria-busy', 'false');
  }
});

try {
  const data = await request('/rag/datasets');
  for (const item of (Array.isArray(data) ? data : data.datasets ?? [])) {
    dataset.add(new Option(item.name || item.id, item.id));
  }
} catch {
  status.textContent = 'Список наборов недоступен. Откройте страницу из запущенного Леса.';
}
