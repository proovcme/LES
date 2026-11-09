import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import App from './App.svelte';
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
  vi.stubGlobal('fetch', vi.fn(async (path: string) => {
    if (path.includes('/by-id/doc-one/viewer')) return new Response('<p>Исходное доказательство</p>', { headers: { 'Content-Type': 'text/html' } });
    if (path.includes('/by-id/doc-one')) return json({ document: { id: 'doc-one', file_name: 'Тест с пробелами.md' } });
    throw new Error(`Unexpected fixture request: ${path}`);
  }));
  component = mount(App, { target: document.body, props: { workspace } });
});
afterEach(async () => { await unmount(component); document.body.innerHTML = ''; vi.unstubAllGlobals(); });

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
  original.click(); await settle(() => !!document.querySelector('iframe'));
  expect(document.querySelector('iframe')?.getAttribute('src')).toContain('doc_name=%D0%A2%D0%B5');
  expect(document.querySelector('iframe')?.getAttribute('sandbox')).not.toContain('allow-same-origin');
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
  await settle(() => !!document.querySelector('iframe'));
  const url = new URL(document.querySelector('iframe')!.src);
  expect(url.searchParams.get('sheet')).toBe('Часть 2');
  expect(url.searchParams.get('locator')).toBe('Часть 2!R2003');
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
  }] } }];
  flushSync();
  document.querySelector<HTMLButtonElement>('[data-citation]')!.click();
  await settle(() => !!document.querySelector('dialog .source-actions'));
  expect(document.querySelector('dialog .eyebrow')?.textContent).toContain('таблица 1, строка 502');
  const original = [...document.querySelectorAll<HTMLButtonElement>('dialog button')].find((button) => button.textContent?.includes('Показать оригинал'))!;
  original.click();
  await settle(() => !!document.querySelector('iframe'));
  expect(new URL(document.querySelector('iframe')!.src).searchParams.get('locator')).toBe('t0r501');
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
