import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { Workspace } from './workspace.svelte';
import { emptyScope } from './contracts';

const sid = '11111111-1111-4111-8111-111111111111';
const session = { session_id: sid, project_id: null, title: 'Диалог', scope: { ...emptyScope(), scope_type: 'datasets', dataset_ids: ['demo'] }, role: 'agent', registered: true, updated_at: '2026-10-31' };
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status });
let workspace: Workspace; let writes: { text: string; revision: number }[];
let stream: (init: RequestInit) => Response;
beforeEach(() => {
  history.replaceState(null, '', '/app/'); workspace = new Workspace(); writes = [];
  stream = () => new Response('event: progress\ndata: {"label":"Ищу"}\n\nevent: token\ndata: "Ответ"\n\nevent: final\ndata: {"answer":"Ответ","source_map":[]}\n\n', { headers: { 'Content-Type': 'text/event-stream' } });
  vi.stubGlobal('fetch', vi.fn(async (path: string, init: RequestInit = {}) => {
    if (path === '/api/light/frontend') return json({ legacy_url: null });
    if (path === '/api/model-connections/effective') return json({ roles: { answer: { model_id: 'demo-model', connection_id: 'demo-connection' } } });
    if (path === '/api/model-connections') return json({ connections: [{ connection_id: 'demo-connection', extension_type: 'ollama' }] });
    if (path === '/api/documents/datasets') return json({ datasets: [{ id: 'demo', name: 'Учебные документы' }] });
    if (path === '/api/workspace/sessions') return json({ sessions: [session] });
    if (path.endsWith('/draft')) {
      if (init.method === 'PUT') { const body = JSON.parse(String(init.body)); writes.push(body); return json({ ...body, revision: body.revision + 1 }); }
      return json({ revision: 0, text: '', attachment: null });
    }
    if (path.startsWith('/api/chat/history')) return json([]);
    if (path === `/api/workspace/sessions/${sid}`) return json(session);
    if (path === '/api/chat/stream') return stream(init);
    throw new Error(`Unexpected fixture request ${path}`);
  }));
});
afterEach(() => { workspace.dispose(); vi.unstubAllGlobals(); });

it('sends the explicit saved scope and optional reranker through the real contract', async () => {
  await workspace.initialize(); workspace.text = 'Вопрос'; workspace.reranker = true;
  await workspace.send(); await workspace.persist();
  const call = vi.mocked(fetch).mock.calls.find(([path]) => path === '/api/chat/stream')!;
  const body = JSON.parse(String(call[1]!.body));
  expect(body.scope.dataset_ids).toEqual(['demo']); expect(body.reranker_enabled).toBe(true);
  expect(body.session_id).toBe(sid); expect(body).not.toHaveProperty('provider_config');
  expect(workspace.messages.at(-1)?.text).toBe('Ответ'); expect(workspace.text).toBe('');
  expect(writes[0].text).toBe('Вопрос'); expect(writes.at(-1)?.text).toBe('');
});

it('retains partial answer and restores the question on a broken connection', async () => {
  stream = () => new Response('event: progress\ndata: {}\n\nevent: token\ndata: "Часть ответа"\n\n', { headers: { 'Content-Type': 'text/event-stream' } });
  await workspace.initialize(); workspace.text = 'Не потеряй вопрос'; await workspace.send();
  expect(workspace.messages.at(-1)?.text).toBe('Часть ответа'); expect(workspace.messages.at(-1)?.meta?.partial).toBe(true);
  expect(workspace.text).toBe('Не потеряй вопрос'); expect(workspace.error).toContain('прервалось');
});

it('does not label a blocked final response as a successful answer', async () => {
  stream = () => new Response('event: progress\ndata: {}\n\nevent: final\ndata: {"answer":"Поиск заблокирован","crag_status":"BLOCKED","blocker":{"action":"Восстановите индекс"}}\n\n', { headers: { 'Content-Type': 'text/event-stream' } });
  await workspace.initialize(); workspace.text = 'Повтори после восстановления'; await workspace.send();
  expect(workspace.status).toContain('требует внимания'); expect(workspace.text).toBe('Повтори после восстановления');
  expect(workspace.messages.at(-1)?.meta?.blocker?.action).toBe('Восстановите индекс');
});

it('keeps a new draft typed while the previous question is being answered', async () => {
  stream = () => { workspace.text = 'Следующий вопрос'; return new Response('event: final\ndata: {"answer":"Ответ"}\n\n', { headers: { 'Content-Type': 'text/event-stream' } }); };
  await workspace.initialize(); workspace.text = 'Первый вопрос'; await workspace.send();
  expect(workspace.text).toBe('Следующий вопрос');
});

it('does not send to an unassigned model or change bindings', async () => {
  await workspace.initialize(); workspace.model = null; workspace.text = 'Вопрос'; await workspace.send();
  expect(workspace.error).toContain('не назначена');
  expect(vi.mocked(fetch).mock.calls.some(([path]) => path === '/api/chat/stream' || String(path).includes('/roles/'))).toBe(false);
});

it('flushes the latest characters before the 400ms autosave delay on normal close', async () => {
  await workspace.initialize(); workspace.text = 'Последние символы 🌲'; workspace.changed();
  expect(writes).toHaveLength(0);
  await workspace.prepareClose();
  expect(writes.at(-1)?.text).toBe('Последние символы 🌲'); expect(workspace.closing).toBe(true);
});

it('a final model claim cannot override an incomplete execution receipt', async () => {
  stream = () => new Response('event: final\ndata: {"answer":"Всё сохранено","execution":{"schema":"les.execution-summary.v1","status":"incomplete","notice":"0 из 1","accepted":false}}\n\n', { headers: { 'Content-Type': 'text/event-stream' } });
  await workspace.initialize(); workspace.text = 'Проверь документ'; await workspace.send();
  expect(workspace.status).toContain('требует внимания'); expect(workspace.text).toBe('Проверь документ');
  expect(workspace.messages.at(-1)?.meta?.execution?.status).toBe('incomplete');
});

it('cancels an in-flight response and preserves its partial text', async () => {
  stream = (init) => new Response(new ReadableStream({ start(controller) {
    controller.enqueue(new TextEncoder().encode('event: progress\ndata: {}\n\nevent: token\ndata: "Начало"\n\n'));
    init.signal!.addEventListener('abort', () => controller.error(new DOMException('Stopped', 'AbortError')));
    setTimeout(() => workspace.stop(), 10);
  } }), { headers: { 'Content-Type': 'text/event-stream' } });
  await workspace.initialize(); workspace.text = 'Вопрос'; await workspace.send();
  expect(workspace.busy).toBe(false); expect(workspace.status).toContain('Остановлено');
  expect(workspace.messages.at(-1)?.text).toBe('Начало'); expect(workspace.text).toBe('Вопрос');
});
