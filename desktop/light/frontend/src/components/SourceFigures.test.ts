import { afterEach, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import SourceFigures from './SourceFigures.svelte';
let component: ReturnType<typeof mount> | undefined;
afterEach(async () => { if (component) await unmount(component); document.body.replaceChildren(); vi.unstubAllGlobals(); });
const digest = 'a'.repeat(64), part = 'b'.repeat(64), visible = 'c'.repeat(64);
const url = (sha: string) => `/api/documents/by-id/found/parts/${sha}?source_sha256=${digest}`;
const button = (label: string) => [...document.querySelectorAll('button')].find(b => b.textContent?.trim() === label)!;
it('reads the discovered member and exact visible view; paginates without retaining the previous image', async () => {
  const calls: Record<string, unknown>[] = [];
  vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => {
    const { args } = JSON.parse(String(init.body)); calls.push(args);
    if (args.view === 'parts') return new Response(JSON.stringify({ result: args.offset === 0 ? {
      parts: [{ sha256: part, member: 'word/media/figure.emf', kind: 'image', label: 'Рисунок', url: url(part), views: [{ sha256: visible, url: url(visible) }] }], next_offset: 1, total: 2,
    } : { parts: [], next_offset: null, total: 2 } }));
    return new Response(JSON.stringify({ result: { text: 'Точные подписи 17.25', next_offset: null, visual_verification_required: true } }));
  }));
  component = mount(SourceFigures, { target: document.body, props: { docId: 'found', digest } });
  await vi.waitFor(() => { flushSync(); expect(button('Рисунок')).toBeTruthy(); });
  button('Рисунок').click();
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Точные подписи 17.25'); });
  expect(calls[1]).toMatchObject({ doc_id: 'found', source_sha256: digest, view: 'part', part_id: visible, member: 'word/media/figure.emf', offset: 0 });
  expect(document.querySelector('img')?.getAttribute('src')).toBe(url(visible));
  button('Ещё объекты').click();
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Порция 2'); });
  expect(calls[2]).toMatchObject({ view: 'parts', offset: 1 });
  expect(document.querySelector('img')).toBeNull();
  expect(document.body.textContent).not.toContain('Точные подписи 17.25');
});
it('rejects an external preview without fetching it or showing another figure', async () => {
  const fetch = vi.fn(async () => new Response(JSON.stringify({ result: { parts: [{ sha256: part, label: 'Внешний', kind: 'image', member: 'word/media/figure.png', url: url(part), preview_url: 'https://outside.invalid/image.png' }], total: 1, next_offset: null } })));
  vi.stubGlobal('fetch', fetch);
  component = mount(SourceFigures, { target: document.body, props: { docId: 'found', digest } });
  await vi.waitFor(() => { flushSync(); expect(button('Внешний')).toBeTruthy(); });
  button('Внешний').click();
  await vi.waitFor(() => { flushSync(); expect(document.querySelector('[role="alert"]')).toBeTruthy(); });
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(document.querySelector('img')).toBeNull();
});
it('skips a technical-only page and exposes the next returned picture without showing raw structure', async () => {
  const offsets: number[] = [];
  vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => {
    const { args } = JSON.parse(String(init.body)); offsets.push(args.offset);
    return new Response(JSON.stringify({ result: args.offset === 0 ? { parts: [
      { kind: 'structure', label: 'RAW WORD XML', sha256: part, member: 'structures/private.json', url: url(part) },
    ], next_offset: 16, total: 17 } : { parts: [
      { kind: 'image', label: 'Найденный рисунок', sha256: visible, member: 'word/media/picture.png', url: url(visible) },
    ], next_offset: null, total: 17 } }));
  }));
  component = mount(SourceFigures, { target: document.body, props: { docId: 'found', digest } });
  await vi.waitFor(() => { flushSync(); expect(button('Найденный рисунок')).toBeTruthy(); });
  expect(offsets).toEqual([0, 16]);
  expect(document.body.textContent).not.toContain('RAW WORD XML');
});
