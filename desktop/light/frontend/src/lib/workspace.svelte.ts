import { ApiError, errorMessage, request, streamChat } from './api';
import type { Attachment, ChatPayload, Dataset, Draft, Message, ModelRole, Scope, Session } from './contracts';
import { sourcesFor } from './contracts';
import { DraftWriter } from './drafts';

export class Workspace {
  session = $state<Session | null>(null);
  sessions = $state<Session[]>([]);
  datasets = $state<Dataset[]>([]);
  messages = $state<Message[]>([]);
  text = $state('');
  attachment = $state<Attachment | null>(null);
  model = $state<ModelRole | null>(null);
  service = $state('');
  legacyURL = $state<string | null>(null);
  loading = $state(true);
  busy = $state(false);
  closing = $state(false);
  beforeClose?: () => Promise<void>;
  uploading = $state(false);
  error = $state('');
  notice = $state('');
  status = $state('');
  draftStatus = $state('');
  draftConflict = $state<Draft | null>(null);
  elapsed = $state(0);
  reranker = $state(false);
  private writer?: DraftWriter;
  private timer?: ReturnType<typeof setTimeout>;
  private controller?: AbortController;
  private alive = true;
  private initializing = false;
  private generationFinished?: Promise<void>;
  private finishGeneration?: () => void;

  async initialize(): Promise<void> {
    if (this.initializing) return;
    this.initializing = true; this.loading = true;
    try {
      const results = await Promise.allSettled([
        request<{ legacy_url: string | null }>('/api/light/frontend'),
        request<{ roles: { answer: ModelRole | null } }>('/api/model-connections/effective'),
        request<{ datasets: Dataset[] }>('/api/documents/datasets'),
        this.refreshSessions(),
        request<{ connections: { connection_id: string; extension_type: string | null }[] }>('/api/model-connections'),
      ]);
      if (!this.alive) return;
      const [config, models, datasets] = results;
      if (config.status === 'fulfilled') this.legacyURL = config.value.legacy_url;
      if (models.status === 'fulfilled') this.model = models.value.roles.answer;
      else this.notice = 'Не удалось проверить назначение модели. Откройте подключения перед отправкой.';
      if (datasets.status === 'fulfilled') this.datasets = datasets.value.datasets;
      else this.notice = 'Не удалось загрузить наборы документов. Повторите запуск чата перед выбором источников.';
      // Session history is essential: do not hide its failure behind a new empty chat.
      if (results[3].status === 'rejected') throw results[3].reason;
      if (results[4].status === 'fulfilled') {
        const connection = results[4].value.connections.find((item) => item.connection_id === this.model?.connection_id);
        const names: Record<string, string> = { ollama: 'Ollama', mlx: 'MLX', lm_studio: 'LM Studio', llama_cpp: 'llama.cpp', lemonade: 'Lemonade', freetoken: 'FreeToken' };
        this.service = connection ? names[connection.extension_type || ''] || 'Совместимый API' : '';
      }
      const requested = new URL(location.href).searchParams.get('session');
      if (requested) await this.openSession(requested);
      else if (this.sessions.length) await this.openSession(this.sessions[0].session_id);
      else await this.newSession();
    } catch (error) { this.error = errorMessage(error); }
    finally { this.loading = false; this.initializing = false; }
  }

  async refreshSessions(): Promise<void> {
    const result = await request<{ sessions: Session[] }>('/api/workspace/sessions');
    if (this.alive) this.sessions = result.sessions;
  }

  changed(): void {
    clearTimeout(this.timer);
    this.draftStatus = 'Сохраняю черновик…';
    this.timer = setTimeout(() => { void this.persist().catch(() => undefined); }, 400);
  }

  async persist(): Promise<void> {
    clearTimeout(this.timer);
    const writer = this.writer;
    if (!writer) return;
    try {
      await writer.save(this.text, this.attachment);
      if (this.alive && this.writer === writer) this.draftStatus = 'Черновик сохранён';
    } catch (error) {
      if (this.alive && this.writer === writer) this.draftStatus = `Черновик пока в этом окне. ${errorMessage(error)}`;
      if (error instanceof ApiError && error.status === 409 && this.session) {
        this.draftConflict = await request<Draft>(`/api/workspace/sessions/${encodeURIComponent(this.session.session_id)}/draft`).catch(() => null);
      }
      throw error;
    }
  }

  async resolveDraft(keepLocal: boolean): Promise<void> {
    if (!this.session || !this.draftConflict) return;
    const remote = this.draftConflict;
    this.writer = new DraftWriter(this.session.session_id, remote);
    if (!keepLocal) { this.text = remote.text; this.attachment = remote.attachment; }
    this.draftConflict = null;
    await this.persist();
  }

  async openSession(id: string): Promise<void> {
    if (this.busy || this.uploading || this.closing) return;
    await this.persist();
    const [session, messages, draft] = await Promise.all([
      request<Session>(`/api/workspace/sessions/${encodeURIComponent(id)}`),
      request<Message[]>(`/api/chat/history?session_id=${encodeURIComponent(id)}`),
      request<Draft>(`/api/workspace/sessions/${encodeURIComponent(id)}/draft`),
    ]);
    if (!this.alive) return;
    this.session = session; this.messages = messages;
    this.text = draft.text; this.attachment = draft.attachment;
    this.writer = new DraftWriter(id, draft); this.error = ''; this.status = '';
    this.draftStatus = draft.text || draft.attachment ? 'Черновик сохранён' : '';
    const url = new URL(location.href); url.searchParams.set('session', id);
    history.replaceState(null, '', url);
  }

  async newSession(): Promise<void> {
    if (this.busy || this.uploading || this.closing) return;
    await this.persist();
    const session = await request<Session>('/api/workspace/sessions', { method: 'POST', body: '{}' });
    await this.refreshSessions(); await this.openSession(session.session_id);
  }

  async setScope(scope: Scope): Promise<void> {
    if (!this.session || this.busy) return;
    this.session = await request<Session>(`/api/workspace/sessions/${encodeURIComponent(this.session.session_id)}`, {
      method: 'PATCH', body: JSON.stringify({ scope }),
    });
  }

  async attach(file: File): Promise<void> {
    if (this.uploading || this.busy || this.closing || !this.session) return;
    if (file.size > 50 * 1024 * 1024) { this.error = 'Для чата выберите файл до 50 МБ. Большие документы добавляйте в набор.'; return; }
    this.uploading = true; this.error = '';
    try {
      const body = new FormData(); body.append('file', file);
      const attached = await request<Attachment>('/api/chat/attachments', {
        method: 'POST', body, headers: { 'Idempotency-Key': crypto.randomUUID() }, signal: AbortSignal.timeout(180000),
      });
      if (!this.alive) return;
      this.attachment = { attachment_id: attached.attachment_id, name: attached.name };
      await this.persist();
    } catch (error) { this.error = errorMessage(error); }
    finally { this.uploading = false; }
  }

  async send(): Promise<void> {
    if (this.busy || this.uploading || this.closing || !this.session || !this.text.trim()) return;
    if (this.text.length > 20000) { this.error = 'Вопрос длиннее 20 000 символов. Сократите его или прикрепите файл.'; return; }
    if (!this.model || this.model.status === 'blocked') {
      this.error = 'Модель для ответов не назначена или недоступна. Откройте подключения и выберите модель.'; return;
    }
    this.busy = true; this.error = ''; this.status = 'Передаю вопрос…'; this.elapsed = 0;
    this.generationFinished = new Promise((resolve) => { this.finishGeneration = resolve; });
    const question = this.text; const attachment = this.attachment;
    const session = this.session;
    let reply = -1; let acknowledged = false;
    this.controller = new AbortController();
    const clock = setInterval(() => { this.elapsed += 1; }, 1000);
    try {
      await this.persist();
      if (this.controller.signal.aborted) throw new DOMException('Stopped', 'AbortError');
      this.messages.push({ role: 'user', text: question }, { role: 'ai', text: '', meta: {} });
      reply = this.messages.length - 1;
      await streamChat({ question, session_id: session.session_id, project_id: session.project_id,
        scope: session.scope, mode: session.role, reranker_enabled: this.reranker, attachment_id: attachment?.attachment_id,
      }, this.controller.signal, (item) => {
        if (!this.alive) return;
        if (!acknowledged) {
          acknowledged = true;
          if (this.text === question && this.attachment?.attachment_id === attachment?.attachment_id) {
            this.text = ''; this.attachment = null; this.changed();
          }
        }
        if (item.event === 'progress') this.status = (item.data as { label?: string }).label || 'Работаю с источниками…';
        else if (item.event === 'token') { this.status = 'Пишу ответ…'; this.messages[reply].text += String(item.data ?? ''); }
        else if (item.event === 'reset') { this.messages[reply].text = ''; this.status = 'Повторно готовлю ответ…'; }
        else if (item.event === 'sources') {
          const payload = item.data as ChatPayload;
          this.messages[reply].meta = { source_map: sourcesFor(payload) };
        } else if (item.event === 'final') {
          const payload = item.data as ChatPayload;
          this.messages[reply] = { role: 'ai', text: payload.answer ?? payload.response ?? this.messages[reply].text,
            srcs: payload.sources, crag: payload.crag_status,
            meta: { source_map: sourcesFor(payload), partial: payload.partial, completion_status: payload.completion_status,
              retrieval_trace: payload.retrieval_trace, blocker: payload.blocker, total_status: payload.total_status,
              execution: payload.execution ?? payload.retrieval_trace?.execution } };
          const incomplete = payload.partial || payload.crag_status === 'BLOCKED' || payload.total_status === 'blocked'
            || (this.messages[reply].meta?.execution?.status === 'incomplete');
          this.status = incomplete ? 'Ответ требует внимания. Проверьте сообщение ниже.' : 'Ответ готов';
          if (incomplete && !this.text) { this.text = question; this.attachment = attachment; this.changed(); }
        }
      });
      if (!session.title) {
        // A title is metadata, not a summary generated by another hidden model.
        const updated = await request<Session>(`/api/workspace/sessions/${encodeURIComponent(session.session_id)}`, {
          method: 'PATCH', body: JSON.stringify({ title: question.trim().slice(0, 80) }),
        }).catch(() => null);
        if (updated) this.session = updated;
      }
      await this.refreshSessions().catch(() => undefined);
    } catch (error) {
      if (reply >= 0) this.messages[reply].meta = { ...this.messages[reply].meta, partial: true, completion_status: 'interrupted' };
      this.status = this.controller.signal.aborted ? 'Остановлено. Полученный текст сохранён.' : 'Ответ не завершён';
      if (!this.controller.signal.aborted) this.error = errorMessage(error);
      if (!this.text) { this.text = question; this.attachment = attachment; this.changed(); }
      if (error instanceof ApiError && error.status === 409) this.notice = 'Проверьте состояние индекса в разделе «Данные».';
    } finally {
      clearInterval(clock); this.busy = false; this.controller = undefined;
      if (acknowledged) void this.persist().catch(() => undefined);
      this.finishGeneration?.(); this.finishGeneration = undefined; this.generationFinished = undefined;
    }
  }

  stop(): void { this.controller?.abort(); }
  async prepareClose(): Promise<void> {
    if (this.uploading) throw new ApiError('Файл ещё читается. Дождитесь завершения и повторите закрытие.');
    this.closing = true; clearTimeout(this.timer);
    try {
      const generation = this.generationFinished;
      this.stop();
      await generation;
      await this.persist();
      await this.writer?.flush();
      await this.beforeClose?.();
    } catch (error) { this.closing = false; throw error; }
  }
  dispose(): void {
    // Route changes inside this SPA do not destroy the workspace. A real close
    // disconnects the stream, which checkpoints the partial answer on the API.
    clearTimeout(this.timer); this.controller?.abort(); this.alive = false;
  }
}
