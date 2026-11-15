import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import SourcePanel from './SourcePanel.svelte';
import { sourceOrigin, type Source } from '../lib/contracts';

let component: ReturnType<typeof mount> | undefined;
beforeEach(() => { HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; this.dispatchEvent(new Event('close')); }; });
afterEach(async () => { if (component) await unmount(component); document.body.replaceChildren(); vi.unstubAllGlobals(); });

it('offers each shared original with its own page, revision and reading cursor', async () => {
  const alias = { doc_id: 'other', dataset_id: 'scope', doc_name: 'Other.docx',
    source_sha256: 'b'.repeat(64), evidence_kind: 'physical_word_section', ordinal: 17,
    read_offset: 21, view_sha256: 'c'.repeat(64) };
  const first: Source = { index: 1, doc_id: 'first', doc_name: 'First.pdf', page: 7,
    dataset_id: 'scope', source_sha256: 'a'.repeat(64), quote: 'Exact shared text', equivalent_sources: [alias] };
  const selectOrigin = vi.fn();
  vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ document: { id: 'first', file_name: 'First.pdf' } }))));
  component = mount(SourcePanel, { target: document.body, props: { source: first, close: vi.fn(), selectOrigin } });
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Тот же текст в других оригиналах (1)'); });
  const details = document.querySelector('details')!; details.open = true;
  [...details.querySelectorAll('button')].find(b => b.textContent?.includes('Other.docx'))!.click();
  expect(selectOrigin).toHaveBeenCalledWith(alias);
  const selected = sourceOrigin(first, alias);
  expect(selected).toMatchObject({ ...alias, index: 1, quote: 'Exact shared text' });
  expect(selected.equivalent_sources).toHaveLength(1);
  expect(selected.equivalent_sources![0].doc_id).toBe('first');
  const returned = sourceOrigin(selected, selected.equivalent_sources![0]);
  expect(returned).toMatchObject({ doc_id: 'first', page: 7, source_sha256: 'a'.repeat(64), index: 1 });
});

it('passes a physical Word citation to the shared reader without losing its unit or revision', async () => {
  const close = vi.fn();
  const read = vi.fn();
  vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
    if (url.startsWith('/api/documents/by-id/')) return new Response(JSON.stringify({ document: { id: 'found-word', file_name: 'Document.docx' } }));
    expect(url).toBe('/api/rag/sources/call');
    const payload = JSON.parse(String(init?.body)); read(payload);
    return new Response(JSON.stringify({ result: { passages: [{ text: 'Цитируемый раздел' }], next_offset: null, view_sha256: 'b'.repeat(64) } }));
  }));
  component = mount(SourcePanel, { target: document.body, props: { close, source: {
    index: 1, doc_id: 'found-word', dataset_id: 'selected', ordinal: 17,
    evidence_kind: 'physical_word_section', view_sha256: 'b'.repeat(64),
    locator: { source_sha256: 'a'.repeat(64) },
  } } });
  await vi.waitFor(() => { flushSync(); expect([...document.querySelectorAll('button')].some(b => b.textContent?.trim() === 'Прочитать текст')).toBe(true); });
  [...document.querySelectorAll('button')].find(b => b.textContent?.trim() === 'Прочитать текст')!.click();
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Цитируемый раздел'); });
  expect(read).toHaveBeenCalledWith({ tool: 'read_source', args: { doc_id: 'found-word', dataset_id: 'selected',
    source_sha256: 'a'.repeat(64), offset: 17, max_chars: 12000, view_sha256: 'b'.repeat(64) } });
  [...document.querySelectorAll('button')].find(b => b.textContent?.trim() === 'Вернуться к ответу')!.click();
  expect(close).toHaveBeenCalledOnce();
});

it('opens a compact PDF identity at its original character position with the same revision pin', async () => {
  const read = vi.fn(); const digest = 'c'.repeat(64); const view = 'd'.repeat(64);
  vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
    if (url.startsWith('/api/documents/by-id/')) {
      expect(new URL(url, 'http://localhost').searchParams.get('source_sha256')).toBe(digest);
      return new Response(JSON.stringify({ document: { id: 'found-pdf', file_name: 'Source.pdf' } }));
    }
    expect(url).toBe('/api/rag/sources/call'); read(JSON.parse(String(init?.body)));
    return new Response(JSON.stringify({ result: { text: 'Найденная позиция 117.250', next_offset: null, view_sha256: view } }));
  }));
  component = mount(SourcePanel, { target: document.body, props: { close: vi.fn(), source: {
    index: 1, doc_id: 'found-pdf', dataset_id: 'selected', page: 7, source_sha256: digest,
    evidence_kind: 'physical_pdf_unit', read_offset: 21000, view_sha256: view,
  } } });
  await vi.waitFor(() => { flushSync(); expect([...document.querySelectorAll('button')].some(b => b.textContent?.trim() === 'Прочитать текст')).toBe(true); });
  expect(document.body.textContent).not.toContain('редакция файла не сохранена');
  const download = [...document.querySelectorAll('a')].find(a => a.textContent?.trim() === 'Скачать')!;
  expect(new URL(download.href, 'http://localhost').searchParams.get('source_sha256')).toBe(digest);
  [...document.querySelectorAll('button')].find(b => b.textContent?.trim() === 'Прочитать текст')!.click();
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Найденная позиция 117.250'); });
  expect(read).toHaveBeenCalledWith({ tool: 'read_pdf_source', args: { doc_id: 'found-pdf', dataset_id: 'selected',
    page: 7, source_sha256: digest, offset: 21000, max_chars: 12000, view_sha256: view } });
});
