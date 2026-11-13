import { afterEach, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import SourceReader from './SourceReader.svelte';
let component: ReturnType<typeof mount> | undefined;
afterEach(async () => { if (component) await unmount(component); document.body.replaceChildren(); vi.unstubAllGlobals(); });
const button = (name: string) => [...document.querySelectorAll('button')].find(b => b.textContent?.trim() === name)!;
it('reads the same source API as MCP, pins continuations and keeps errors visible', async () => {
  const calls: { tool: string; args: Record<string, unknown> }[] = [];
  vi.stubGlobal('fetch', vi.fn(async (url: string, init: RequestInit) => {
    expect(url).toBe('/api/rag/sources/call');
    const value = JSON.parse(String(init.body)); calls.push(value);
    if (calls.length === 2) return new Response(JSON.stringify({ detail: 'Редакция изменилась.', code: 'SOURCE_VIEW_CHANGED' }), { status: 409 });
    return new Response(JSON.stringify({ result: { text: '|Позиция|кВт|\n|---|---|\n|Насос|17.25|',
      ocr: true, next_offset: calls.length === 1 ? 48 : null, view_sha256: 'b'.repeat(64) } }));
  }));
  component = mount(SourceReader, { target: document.body, props: { docId: 'found-by-search', datasetId: 'selected', digest: 'a'.repeat(64), page: 7, pdf: true } });
  await vi.waitFor(() => { flushSync(); expect(document.querySelector('table')?.textContent).toContain('Насос'); });
  expect(document.body.textContent).toContain('Текст распознан со скана');
  button('Читать дальше').click();
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Редакция изменилась.'); });
  expect(document.body.textContent).not.toContain('Все порции текста страницы показаны.');
  expect(calls[1]).toEqual({ tool: 'read_pdf_source', args: { doc_id: 'found-by-search', dataset_id: 'selected',
    source_sha256: 'a'.repeat(64), page: 7, max_chars: 12000, offset: 48, view_sha256: 'b'.repeat(64) } });
});
it('does not claim a Word document is complete after a failed return to an earlier portion', async () => {
  let count = 0;
  vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => {
    const { tool, args } = JSON.parse(String(init.body));
    expect(tool).toBe('read_source');
    count++;
    if (count === 3) return new Response(JSON.stringify({ code: 'SOURCE_REVISION_CHANGED', detail: 'Оригинал изменился.' }), { status: 409 });
    return new Response(JSON.stringify({ result: { passages: [{ text: `|Позиция|кВт|\n|---|---|\n|Насос ${args.offset}|17.25|` }],
      ocr: true, unread_visual_count: 2, next_offset: count === 1 ? 1 : null, view_sha256: 'c'.repeat(64) } }));
  }));
  component = mount(SourceReader, { target: document.body, props: { docId: 'word', page: 1, pdf: false } });
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Насос 0'); });
  expect(document.body.textContent).toContain('В документе распознаны встроенные изображения.');
  expect(document.body.textContent).toContain('Объектов без распознанного текста: 2.');
  button('Читать дальше').click();
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Все порции извлечённого текста показаны.'); });
  button('Предыдущая порция').click();
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Оригинал изменился.'); });
  expect(document.body.textContent).not.toContain('Все порции извлечённого текста показаны.');
});
it('opens the cited Word unit in the pinned view and can return to the beginning', async () => {
  const offsets: number[] = [];
  vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => {
    const { tool, args } = JSON.parse(String(init.body));
    expect(tool).toBe('read_source');
    expect(args.view_sha256).toBe(args.offset === 0 ? undefined : 'd'.repeat(64));
    offsets.push(args.offset);
    return new Response(JSON.stringify({ result: { passages: [{ text: `Строка ${args.offset}` }],
      next_offset: args.offset === 17 ? 19 : null, view_sha256: 'd'.repeat(64) } }));
  }));
  component = mount(SourceReader, { target: document.body, props: { docId: 'word', page: 1,
    pdf: false, initialOffset: 17, viewSHA: 'd'.repeat(64) } });
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Строка 17'); });
  button('Читать дальше').click();
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Строка 19'); });
  expect(document.body.textContent).toContain('Достигнут конец извлечённого текста.');
  expect(document.body.textContent).not.toContain('Все порции извлечённого текста показаны.');
  button('Предыдущая порция').click();
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Строка 17'); });
  button('К началу документа').click();
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Строка 0'); });
  expect(offsets).toEqual([17, 19, 17, 0]);
});
