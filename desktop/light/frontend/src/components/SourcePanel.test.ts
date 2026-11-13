import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import SourcePanel from './SourcePanel.svelte';

let component: ReturnType<typeof mount> | undefined;
beforeEach(() => { HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; this.dispatchEvent(new Event('close')); }; });
afterEach(async () => { if (component) await unmount(component); document.body.replaceChildren(); vi.unstubAllGlobals(); });

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
