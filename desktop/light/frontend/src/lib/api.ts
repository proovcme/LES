import type { StreamEvent } from './contracts';

export class ApiError extends Error {
  constructor(message: string, public status = 0, public code = '') { super(message); }
}

export function errorMessage(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  return 'Не удалось связаться с Лесом. Проверьте запуск приложения и повторите попытку. Ваш текст сохранён.';
}

async function check(response: Response): Promise<Response> {
  if (response.ok) return response;
  let body: { detail?: unknown; code?: string } = {};
  try { body = await response.json(); } catch { /* HTML from an unavailable service is not user text. */ }
  const nested = body.detail && typeof body.detail === 'object' ? body.detail as { message?: unknown; code?: string } : null;
  const detail = typeof body.detail === 'string' ? body.detail : typeof nested?.message === 'string' ? nested.message : undefined;
  const message = response.status === 401 ? 'Нужно войти в Лес. Откройте прежний интерфейс и проверьте доступ.'
    : response.status === 403 ? 'Недостаточно прав для этого действия.'
    : response.status === 422 ? detail ?? 'Проверьте заполненные поля. Вопрос — не больше 20 000 символов.'
    : detail ?? 'Лес не смог выполнить действие. Повторите попытку; ваш текст сохранён.';
  throw new ApiError(message, response.status, body.code ?? nested?.code);
}

export async function request<T>(path: string, options: RequestInit = {}): Promise<T> {
  const headers = new Headers(options.headers);
  if (options.body && !(options.body instanceof FormData)) headers.set('Content-Type', 'application/json');
  const response = await fetch(path, { ...options, headers, credentials: 'same-origin', signal: options.signal ?? AbortSignal.timeout(15000) });
  await check(response);
  return response.json() as Promise<T>;
}

/** Incremental UTF-8/SSE decoder: boundaries may bisect Cyrillic, CRLF or JSON. */
export class SSEDecoder {
  private buffer = '';
  private decoder = new TextDecoder();
  constructor(private receive: (event: StreamEvent) => void) {}
  push(bytes?: Uint8Array, done = false): void {
    this.buffer += this.decoder.decode(bytes, { stream: !done });
    let match: RegExpExecArray | null;
    while ((match = /\r?\n\r?\n/.exec(this.buffer))) {
      const frame = this.buffer.slice(0, match.index);
      if (frame.length > 4 * 1024 * 1024) throw new ApiError('Ответ слишком велик для одного сообщения. Уточните запрос.');
      this.buffer = this.buffer.slice(match.index + match[0].length);
      let event = 'message'; const data: string[] = [];
      for (const line of frame.split(/\r?\n/)) {
        if (line.startsWith('event:')) event = line.slice(6).trim();
        if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
      }
      if (data.length) {
        try { this.receive({ event, data: JSON.parse(data.join('\n')) }); }
        catch (error) {
          if (error instanceof SyntaxError) throw new ApiError('Лес прислал повреждённый ответ. Полученный текст сохранён; повторите запрос.');
          throw error;
        }
      }
    }
    if (this.buffer.length > 4 * 1024 * 1024) throw new ApiError('Ответ слишком велик для одного сообщения. Уточните запрос.');
    if (done && this.buffer.trim()) throw new ApiError('Соединение прервалось до завершения ответа. Полученный текст сохранён.');
  }
}

export async function streamChat(body: object, signal: AbortSignal, receive: (event: StreamEvent) => void): Promise<void> {
  const response = await check(await fetch('/api/chat/stream', {
    method: 'POST', credentials: 'same-origin', signal,
    headers: { 'Content-Type': 'application/json', Accept: 'text/event-stream' }, body: JSON.stringify(body),
  }));
  if (!response.headers.get('content-type')?.includes('text/event-stream') || !response.body) {
    throw new ApiError('Сервис чата не прислал поток ответа. Проверьте подключение и повторите запрос.');
  }
  let terminal = false;
  const decoder = new SSEDecoder((item) => {
    if (terminal) return;
    if (item.event === 'error') {
      const error = item.data as { detail?: string; status?: number; code?: string };
      throw new ApiError(error.detail || 'Не удалось завершить ответ. Повторите запрос.', error.status, error.code);
    }
    if (item.event === 'final') terminal = true;
    receive(item);
  });
  const reader = response.body.getReader();
  try {
    while (true) {
      const { value, done } = await reader.read();
      decoder.push(value, done);
      if (done || terminal) break;
    }
    if (!terminal) throw new ApiError('Соединение прервалось до завершения ответа. Полученный текст сохранён.');
  } finally {
    await reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}
