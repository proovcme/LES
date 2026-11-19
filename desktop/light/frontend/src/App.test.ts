import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import App from './App.svelte';
import StatusPage from './components/StatusPage.svelte';
import { Workspace } from './lib/workspace.svelte';
import { emptyScope } from './lib/contracts';

let component: ReturnType<typeof mount>; let workspace: Workspace;
const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { 'Content-Type': 'application/json' } });
async function settle(predicate: () => boolean) {
  for (let i = 0; i < 100; i++) { flushSync(); if (predicate()) return; await new Promise((resolve) => setTimeout(resolve, 2)); }
  throw new Error('DOM did not reach the expected state');
}
beforeEach(() => {
  history.replaceState(null, '', '/app/');
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; this.dispatchEvent(new Event('close')); };
  workspace = new Workspace();
  workspace.initialize = vi.fn(async () => {
    workspace.loading = false;
    workspace.session = { session_id: 'demo', title: 'Учебный диалог', scope: emptyScope(), project_id: null, role: 'agent', registered: true, updated_at: '2026-10-31' };
    workspace.model = { model_id: 'Учебная модель', connection_id: 'demo' };
    workspace.text = 'Мой черновик';
    workspace.messages = [{ role: 'ai', text: 'Подтверждено [1].', meta: { source_map: [{ index: 1, doc_id: 'doc-one', dataset_id: 'demo', doc_name: 'Тест с пробелами.md', snippet: 'Исходное доказательство' }] } }];
  });
  vi.stubGlobal('fetch', vi.fn(async (path: string, init?: RequestInit) => {
    if (path === '/api/rag/sources/call') {
      const { tool, args } = JSON.parse(String(init?.body));
      const text = tool === 'read_excel_source' ? 'B2003 = 7654321' : args.offset ? 'Столбец 2: 32' : 'Текст оригинала';
      return json({ result: { passages: [{ text }], next_offset: null } });
    }
    if (path.includes('/by-id/doc-one')) {
      const name = new URL(path, 'http://localhost').searchParams.get('doc_name');
      return json({ document: { id: 'doc-one', file_name: name } });
    }
    throw new Error(`Unexpected fixture request: ${path}`);
  }));
  component = mount(App, { target: document.body, props: { workspace } });
});
afterEach(async () => { await unmount(component); document.body.innerHTML = ''; vi.unstubAllGlobals(); });

it('offers indexing status after a folder is queued and saves the current draft before opening it', async () => {
  const persist = vi.spyOn(workspace, 'persist').mockResolvedValue(undefined);
  vi.spyOn(workspace, 'refreshDatasets').mockResolvedValue(undefined);
  vi.stubGlobal('fetch', vi.fn(async (path: string) => {
    if (path === '/api/projects') return json({ projects: [] });
    if (path === '/api/light/folder-watches') return json({ watches: [] });
    if (path === '/api/health') return json({ status: 'ok', rag: { status: 'ready', dense_available: true, qdrant: { ok: true, points: 53169 } } });
    if (path === '/api/model-connections/effective') return json({ roles: { embeddings: { model_id: 'Учебная модель' } } });
    if (path.startsWith('/api/jobs/summary')) return json({ jobs: [
      { id: 'running', status: 'running', phase: 'index', total: 328, indexed: 308, index_pending: 20,
        current_stage: 'EMBED', current_file: 'План.pdf' },
      { id: 'old', status: 'failed', phase: 'index', superseded: 1, source: 'native_external', message: 'Прежний сбой' },
    ] });
    if (path === '/api/rag/datasets') return json({ id: 'folder', name: 'Тест' });
    if (path === '/api/rag/index-external') return json({ intake_job_id: 'queued' });
    throw new Error('Unexpected fixture request: ' + path);
  }));
  const findButton = (name: string) => [...document.querySelectorAll<HTMLButtonElement>('button')]
    .find((button) => button.textContent?.trim() === name);
  await settle(() => findButton('Данные')?.disabled === false);
  findButton('Данные')!.click();
  await settle(() => document.querySelector<HTMLInputElement>('dialog input[placeholder="Вставьте путь к папке"]')?.disabled === false);
  await settle(() => document.querySelector('dialog .status-jobs')?.textContent?.includes('308 из 328') === true);
  expect(document.querySelector('dialog .status-jobs')?.textContent).toContain('Индекс · Поиск доступен');
  expect(document.querySelector('dialog .status-jobs')?.textContent).toContain('Осталось 20');
  expect(document.querySelector('dialog .status-jobs')?.textContent).not.toContain('Прежний сбой');
  expect(findButton('К ходу индексации')).toBeUndefined();
  const input = document.querySelector<HTMLInputElement>('dialog input[placeholder="Вставьте путь к папке"]')!;
  input.value = 'C:/Тест';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await settle(() => findButton('Добавить папку')?.disabled === false);
  findButton('Добавить папку')!.click();
  await settle(() => !!findButton('К ходу индексации'));
  const assign = vi.fn();
  vi.stubGlobal('location', { href: location.href, origin: location.origin, assign });
  findButton('К ходу индексации')!.click();
  await settle(() => assign.mock.calls.length === 1);
  expect(persist).toHaveBeenCalledOnce();
  expect(String(assign.mock.calls[0][0])).toContain('/app/?view=status&session=demo');
});

it('shows why indexing is waiting and retries the original job from the status page', async () => {
  const job = { id: 'folder-job', source: 'native_external', phase: 'index', status: 'waiting',
    message: 'Индексация ждёт свободную оперативную память.', can_retry: 1, total: 328 };
  const retry = vi.fn();
  vi.stubGlobal('fetch', vi.fn(async (path: string, init?: RequestInit) => {
    if (path.startsWith('/api/jobs/summary')) return json({ jobs: [job] });
    if (path === '/api/light/intake' && init?.method === 'POST') {
      retry(JSON.parse(String(init.body)));
      job.status = 'queued';
      job.can_retry = 0;
      return json(job);
    }
    if (path === '/api/model-connections/effective') return json({ roles: {} });
    return json({ status: 'ok' });
  }));
  const panel = mount(StatusPage, { target: document.body });
  const retryButton = () => [...document.querySelectorAll<HTMLButtonElement>('.status-jobs button')]
    .find((button) => button.textContent?.trim() === 'Повторить индексацию');
  try {
    await settle(() => !!retryButton());
    expect(document.querySelector('.status-jobs')?.textContent).toContain(job.message);
    retryButton()!.click();
    await settle(() => document.querySelector('.status-jobs')?.textContent?.includes('В очереди') === true);
    expect(retry).toHaveBeenCalledExactlyOnceWith({ id: job.id });
  } finally {
    await unmount(panel);
  }
});

it('selects a retrieval strategy in answer options and restores the preference', async () => {
  await settle(() => !!document.querySelector('#question'));
  document.querySelector<HTMLButtonElement>('button[aria-label="Параметры ответа"]')!.click();
  await settle(() => !!document.querySelector('dialog select'));
  const modes = document.querySelector<HTMLSelectElement>('dialog select')!;
  expect([...modes.options].map(item => item.value)).toEqual(['pull', 'push', 'hybrid']);
  modes.value = 'hybrid';
  modes.dispatchEvent(new Event('change', { bubbles: true }));
  flushSync();
  try {
    expect(workspace.retrievalMode).toBe('hybrid');
    expect(new Workspace().retrievalMode).toBe('hybrid');
  } finally {
    localStorage.removeItem('les.retrieval-mode');
  }
});

it('uses the paperclip to index a file, waits for publication and preserves the draft', async () => {
  await settle(() => !!document.querySelector('#question'));
  workspace.indexedAttachments = true;
  const receipt = { intake_job_id: 'a'.repeat(32), dataset_id: 'chat-files', name: 'План.pdf' };
  let queued = false; let selected = false;
  const fetcher = vi.fn(async (path: string, init?: RequestInit) => {
    if (path === '/api/rag/attach?mode=index') {
      expect(init?.body).toBeInstanceOf(FormData);
      expect(new Headers(init?.headers).get('Idempotency-Key')).toBeTruthy();
      queued = true; return json(receipt);
    }
    if (path.startsWith('/api/light/intake?')) {
      expect(path).toContain(receipt.intake_job_id);
      return json({ id: receipt.intake_job_id, dataset_id: receipt.dataset_id, status: 'completed', indexed: 1 });
    }
    if (path === '/api/documents/datasets') return json({ datasets: [{ id: 'chat-files', name: 'Вложения чата' }] });
    if (path === '/api/workspace/sessions/demo' && init?.method === 'PATCH') {
      const { scope } = JSON.parse(init.body as string);
      expect(scope.dataset_ids).toEqual(['chat-files']);
      selected = true; return json({ ...workspace.session, scope });
    }
    throw new Error(`Unexpected fixture request: ${path}`);
  });
  vi.stubGlobal('fetch', fetcher);
  flushSync();
  document.querySelector<HTMLButtonElement>('[aria-label="Прикрепить файл"]')!.click();
  await settle(() => !!document.querySelector('dialog'));
  const button = [...document.querySelectorAll<HTMLButtonElement>('dialog button')].find((b) => b.textContent === 'Добавить в поиск')!;
  button.click();
  const input = document.querySelector<HTMLInputElement>('input[type=file]')!;
  Object.defineProperty(input, 'files', { value: [new File(['sample'], receipt.name, { type: 'application/pdf' })] });
  input.dispatchEvent(new Event('change', { bubbles: true }));
  await settle(() => document.body.textContent!.includes('Файл готов к поиску'));
  expect(queued).toBe(true);
  expect(workspace.attachment).toBeNull();
  expect(workspace.session?.scope.scope_type).toBe('none');
  const search = [...document.querySelectorAll<HTMLButtonElement>('button')].find((b) => b.textContent === 'Искать во вложениях')!;
  search.click();
  await settle(() => selected && !workspace.indexReceipt);
  expect(document.querySelector<HTMLTextAreaElement>('#question')?.value).toBe('Мой черновик');
});

it('opens evidence and its original, then returns with draft, scroll and focus preserved', async () => {
  await settle(() => !!document.querySelector('[data-citation]'));
  const transcript = document.querySelector<HTMLDivElement>('.transcript')!; transcript.scrollTop = 123;
  const citation = document.querySelector<HTMLButtonElement>('[data-citation]')!; citation.focus(); citation.click();
  await settle(() => !!document.querySelector('dialog .source-actions'));
  expect(document.querySelector('blockquote')?.textContent).toContain('Исходное доказательство');
  const original = [...document.querySelectorAll('dialog button')].find((button) => button.textContent?.includes('Показать оригинал')) as HTMLButtonElement;
  original.click(); await settle(() => document.querySelector('.source-reading')?.textContent?.includes('Текст оригинала') === true);
  expect(document.querySelector('dialog a[download]')?.getAttribute('href')).toContain('doc_name=%D0%A2%D0%B5');
  const back = [...document.querySelectorAll('dialog button')].find((button) => button.textContent?.includes('Вернуться к ответу')) as HTMLButtonElement;
  back.click(); await settle(() => !document.querySelector('dialog'));
  expect(document.querySelector<HTMLTextAreaElement>('#question')?.value).toBe('Мой черновик');
  expect(transcript.scrollTop).toBe(123); expect(document.activeElement).toBe(citation);
});

it('opens a table citation on its exact sheet and row', async () => {
  await settle(() => !!document.querySelector('[data-citation]'));
  workspace.messages = [{ role: 'ai', text: 'Подтверждено [1].', meta: { source_map: [{
    index: 1, doc_id: 'doc-one', dataset_id: 'demo', doc_name: 'Книга.xlsx',
    quote: 'B2003 = 7654321', locator: { sheet: 'Часть 2', row: 2003, cell: 'A2003:B2003' },
  }] } }];
  flushSync();
  document.querySelector<HTMLButtonElement>('[data-citation]')!.click();
  await settle(() => !!document.querySelector('dialog .source-actions'));
  expect(document.querySelector('dialog .eyebrow')?.textContent).toContain('Часть 2 · A2003:B2003');
  const original = [...document.querySelectorAll<HTMLButtonElement>('dialog button')].find((button) => button.textContent?.includes('Показать оригинал'))!;
  original.click();
  await settle(() => document.querySelector('.source-reading')?.textContent?.includes('B2003 = 7654321') === true);
  const [, request] = vi.mocked(fetch).mock.lastCall!;
  expect(JSON.parse(String(request?.body))).toMatchObject({ tool: 'read_excel_source',
    args: { doc_id: 'doc-one', dataset_id: 'demo', sheet_name: 'Часть 2', row_start: 2003, row_end: 2003, limit: 1 } });
});

it('separates cited revisions from found material and marks a conclusion without sources', async () => {
  await settle(() => !!document.querySelector('[data-citation]'));
  const map = [
    { index: 1, doc_name: 'Редакция А/Лист.pdf', page: 7 },
    { index: 2, doc_name: 'Редакция Б/Лист.pdf', page: 9 },
  ];
  workspace.messages = [{ role: 'ai', text: 'В первой редакции 10 дней [Источник 1].', meta: { source_map: map } }];
  flushSync();
  expect(document.querySelector('[aria-label="Цитаты в ответе"]')?.textContent).toContain('Редакция А/Лист.pdf');
  expect(document.querySelector('[aria-label="Цитаты в ответе"]')?.textContent).toContain('стр. 7');
  expect(document.querySelector('[aria-label="Найденные материалы"]')?.textContent).toContain('Редакция Б/Лист.pdf');
  workspace.messages = [{ role: 'ai', text: 'Срок составляет 10 дней.', meta: { source_map: map } }];
  flushSync();
  expect(document.querySelector('[aria-label="Цитаты в ответе"]')).toBeNull();
  expect(document.querySelector('.message .notice')?.textContent).toContain('нет ссылки');
});

it('opens a Word citation at its table row', async () => {
  await settle(() => !!document.querySelector('[data-citation]'));
  workspace.messages = [{ role: 'ai', text: 'Подтверждено [1].', meta: { source_map: [{
    index: 1, doc_id: 'doc-one', dataset_id: 'demo', doc_name: 'Ведомость.docx',
    quote: 'Столбец 2: 32', locator: { anchor: 't0r501' },
    evidence_kind: 'physical_word_section', read_offset: 17, view_sha256: 'a'.repeat(64),
  }] } }];
  flushSync();
  document.querySelector<HTMLButtonElement>('[data-citation]')!.click();
  await settle(() => !!document.querySelector('dialog .source-actions'));
  expect(document.querySelector('dialog .eyebrow')?.textContent).toContain('таблица 1, строка 502');
  const original = [...document.querySelectorAll<HTMLButtonElement>('dialog button')].find((button) => button.textContent?.includes('Показать оригинал'))!;
  original.click();
  await settle(() => document.querySelector('.source-reading')?.textContent?.includes('Столбец 2: 32') === true);
  const [, request] = vi.mocked(fetch).mock.lastCall!;
  expect(JSON.parse(String(request?.body))).toMatchObject({ tool: 'read_source',
    args: { doc_id: 'doc-one', dataset_id: 'demo', offset: 17, view_sha256: 'a'.repeat(64) } });
});

it('does not submit during IME composition or Shift+Enter and submits Enter once', async () => {
  await settle(() => !!document.querySelector('#question'));
  workspace.send = vi.fn(async () => undefined);
  const textarea = document.querySelector<HTMLTextAreaElement>('#question')!;
  textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', isComposing: true, bubbles: true, cancelable: true }));
  textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', shiftKey: true, bubbles: true, cancelable: true }));
  expect(workspace.send).not.toHaveBeenCalled();
  textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
  expect(workspace.send).toHaveBeenCalledTimes(1);
});

it('keeps stop accessible when the compact composer hides its text', async () => {
  await settle(() => !workspace.loading);
  workspace.stop = vi.fn(); workspace.busy = true;
  await settle(() => !!document.querySelector('[aria-label="Остановить ответ"]'));
  const stop = document.querySelector<HTMLButtonElement>('[aria-label="Остановить ответ"]')!;
  expect(stop.textContent).toContain('Остановить');
  stop.click(); expect(workspace.stop).toHaveBeenCalledOnce();
});

it('keeps dialog controls keyboard-native and closes through the dialog close event', async () => {
  await settle(() => !workspace.loading);
  const picker = [...document.querySelectorAll('button')].find((button) => button.textContent?.includes('Без набора документов'))!;
  picker.click(); await settle(() => !!document.querySelector('dialog'));
  expect(document.querySelector('dialog input[type=radio]')).not.toBeNull();
  document.querySelector<HTMLDialogElement>('dialog')!.close(); await settle(() => !document.querySelector('dialog'));
  expect(document.querySelector<HTMLTextAreaElement>('#question')?.value).toBe('Мой черновик');
});
