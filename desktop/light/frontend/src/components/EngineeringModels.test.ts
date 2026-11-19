import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import EngineeringModels from './EngineeringModels.svelte';
import EngineeringSource from './EngineeringSource.svelte';
import type { ModelLocator } from '../lib/engineering';
let component: ReturnType<typeof mount> | undefined;
beforeEach(() => {
  localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; this.dispatchEvent(new Event('close')); };
});
afterEach(async () => { if (component) await unmount(component); document.body.replaceChildren(); vi.unstubAllGlobals(); });
const settle = async () => { for (let i = 0; i < 12; i++) { await new Promise(r => setTimeout(r, 0)); flushSync(); } };
const target: ModelLocator = { provider: 'uvyazka', project_id: 'example', dataset_id: 'snapshot', object_id: 'elements:42', native_id: '42',
  source_sha256: 'a'.repeat(64), source_name: 'Example.rvt', section: 'elements', coverage: { status: 'partial' }, approval_status: 'unreviewed' };

it('offers installation and retry when Uvyazka is unavailable without launching an installer', async () => {
  const calls: string[] = [];
  vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => {
    const { action } = JSON.parse(init.body as string); calls.push(action);
    return action === 'connection' ? new Response(JSON.stringify({ url: null }))
      : new Response(JSON.stringify({ code: 'ENGINEERING_UNAVAILABLE', detail: 'Увязка не отвечает.' }), { status: 422 });
  }));
  component = mount(EngineeringModels, { target: document.body, props: { close: vi.fn(), refresh: vi.fn() } }); await settle();
  document.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); await settle();
  expect(document.body.textContent).toContain('Если она уже установлена');
  const install = Array.from(document.querySelectorAll('a')).find(a => a.textContent?.includes('Установить с GitHub'))!;
  expect(install.href).toBe('https://github.com/proovcme/uvyazka/releases'); expect(install.target).toBe('_blank');
  expect(calls).toEqual(['connection', 'sync_projects']); expect(document.querySelector('button[disabled]')).toBeNull();
});

it('restores the saved connection and loads the current project without a selector', async () => {
  const refresh = vi.fn(async () => {}); const actions: string[] = [];
  const url = 'http://127.0.0.1:8765/';
  vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => {
    const body = JSON.parse(init.body as string); actions.push(body.action);
    const value = body.action === 'connection' ? { url }
      : body.action === 'refresh_projects' ? { url, projects: [{ id: 'example', local_id: 1, name: 'Example', available: true }] }
      : body.action === 'datasets' ? { items: [{ dataset_id: 'snapshot', source: { relative_path: 'Example.rvt', format: 'rvt' }, object_count: 1, coverage: { status: 'partial' }, freshness: { status: 'snapshot_metadata_matches' } }], next_offset: null }
      : body.action === 'sources' ? { items: [], next_offset: null }
      : { notice: 'Снимок поставлен в общую очередь индексации.' };
    return new Response(JSON.stringify(value));
  }));
  component = mount(EngineeringModels, { target: document.body, props: { close: vi.fn(), refresh } }); await settle();
  expect(actions).toEqual(['connection', 'refresh_projects', 'sources']);
  expect(document.querySelector('select')).toBeNull();
  (Array.from(document.querySelectorAll('button')).find(b => b.textContent === 'Ранее прочитанное')!).click(); await settle();
  (Array.from(document.querySelectorAll('button')).find(b => b.textContent?.includes('Добавить снимок'))!).click(); await settle();
  expect(actions).toEqual(['connection', 'refresh_projects', 'sources', 'datasets', 'import']); expect(refresh).toHaveBeenCalledOnce();
  expect(document.body.textContent).toContain('очередь индексации'); expect(document.body.textContent).not.toContain('Готово к поиску');
  expect(document.body.textContent).not.toContain('Установить с GitHub');
  const address = document.querySelector('input')!; address.value = 'http://127.0.0.1:8773/'; address.dispatchEvent(new Event('input', { bubbles: true })); await settle();
  expect(document.querySelector('select')).toBeNull(); expect(document.body.textContent).not.toContain('Добавить снимок в поиск');
  expect(actions).toEqual(['connection', 'refresh_projects', 'sources', 'datasets', 'import']);
});
it('opens only a revision-checked object and clears an old link when the address changes', async () => {
  localStorage.setItem('les.uvyazka.url.v1', 'http://127.0.0.1:8765/');
  const calls: unknown[] = [];
  vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => { calls.push(JSON.parse(init.body as string)); return new Response(JSON.stringify({ url: 'http://127.0.0.1:8765/simulation/?les_object=elements%3A42' })); }));
  const withLargeCoverage = { ...target, coverage: { status: 'partial', limitations: ['reader detail '.repeat(3000)] } };
  component = mount(EngineeringSource, { target: document.body, props: { target: withLargeCoverage } }); await settle();
  document.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); await settle();
  expect(calls[0]).toMatchObject({ action: 'viewer', target: { project_id: target.project_id, dataset_id: target.dataset_id, object_id: target.object_id, source_sha256: target.source_sha256 } });
  expect(JSON.stringify(calls[0]).length).toBeLessThan(2000); expect(document.querySelector('a')?.href).toContain('elements%3A42');
  const input = document.querySelector('input')!; input.value = 'http://127.0.0.1:8766/'; input.dispatchEvent(new Event('input', { bubbles: true })); await settle();
  expect(document.querySelector('a')).toBeNull();
});
it('previews without import and reuses the same registered read after timeout', async () => {
  const refresh = vi.fn(async () => {}); const reads: Record<string, unknown>[] = []; const previews: Record<string, unknown>[] = [];
  vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => {
    const body = JSON.parse(init.body as string);
    if (body.action === 'connection') return new Response(JSON.stringify({ url: null }));
    if (body.action === 'sync_projects') return new Response(JSON.stringify({ url: body.url, projects: [{ id: 'example', local_id: 1, name: 'Example', available: true }] }));
    if (body.action === 'sources') return new Response(JSON.stringify({ items: [{ doc_id: 'doc-1', relative_path: 'Model.rvt', format: 'rvt', sha256: 'a'.repeat(64), available: true, missing: false }], next_offset: null }));
    if (body.action === 'preview') { previews.push(body); return new Response(JSON.stringify({ url: 'http://127.0.0.1:8772/simulation/?les_object=%40dataset' })); }
    expect(body.action).toBe('read_import'); reads.push(body);
    return reads.length === 1 ? new Response(JSON.stringify({ code: 'ENGINEERING_READ_TIMEOUT', detail: 'Чтение ещё не завершено.' }), { status: 422 })
      : new Response(JSON.stringify({ notice: 'Снимок поставлен в очередь.' }));
  }));
  component = mount(EngineeringModels, { target: document.body, props: { close: vi.fn(), refresh } }); await settle();
  document.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); await settle();
  expect(reads).toHaveLength(0);
  Array.from(document.querySelectorAll('button')).find(b => b.textContent === 'Просмотр файла')!.click(); await settle();
  expect(reads).toHaveLength(0); expect(refresh).not.toHaveBeenCalled();
  expect(Array.from(document.querySelectorAll('a')).find(a => a.textContent?.includes('Открыть файл в Увязке'))?.href).toContain('les_object=%40dataset');
  const button = Array.from(document.querySelectorAll('button')).find(b => b.textContent === 'Прочитать и добавить в поиск')!;
  button.click(); await settle(); expect(reads).toHaveLength(1); expect(refresh).not.toHaveBeenCalled();
  button.click(); await settle(); expect(reads).toHaveLength(2); expect(refresh).toHaveBeenCalledOnce();
  expect(reads[0]).toEqual(reads[1]); expect(reads[0]).toMatchObject({ doc_id: 'doc-1', expected_source_sha256: 'a'.repeat(64) });
  expect(reads[0].request_id).toBeTruthy(); expect(reads[0]).not.toHaveProperty('path');
  expect(previews[0].request_id).toEqual(reads[0].request_id);
});
