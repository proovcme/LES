import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, SSEDecoder, streamChat } from './api';

afterEach(() => vi.unstubAllGlobals());
const encode = (value: string) => new TextEncoder().encode(value);
function response(text: string) { return new Response(encode(text), { headers: { 'Content-Type': 'text/event-stream' } }); }

describe('stream transport', () => {
  it('decodes split Cyrillic, CRLF, comments and multiline JSON at every byte boundary', () => {
    const events: unknown[] = []; const decoder = new SSEDecoder((event) => events.push(event));
    const bytes = encode(': keepalive\r\n\r\nevent: token\r\ndata: "Лес 🌲 помнит"\r\n\r\nevent: final\ndata: {\ndata: "answer": "Готово"}\n\n');
    for (const byte of bytes) decoder.push(Uint8Array.of(byte));
    decoder.push(undefined, true);
    expect(events).toEqual([{ event: 'token', data: 'Лес 🌲 помнит' }, { event: 'final', data: { answer: 'Готово' } }]);
  });
  it('rejects truncated frames and malformed JSON', () => {
    const decoder = new SSEDecoder(() => undefined);
    decoder.push(encode('event: token\ndata: "начало'));
    expect(() => decoder.push(undefined, true)).toThrow('прервалось');
    expect(() => new SSEDecoder(() => undefined).push(encode('data: broken\n\n'))).toThrow(ApiError);
  });
  it('requires final even if some tokens arrived', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(response('event: token\ndata: "часть"\n\n')));
    const receive = vi.fn();
    await expect(streamChat({}, new AbortController().signal, receive)).rejects.toThrow('прервалось');
    expect(receive).toHaveBeenCalledWith({ event: 'token', data: 'часть' });
  });
  it('preserves reset and final; ignores trailing events after final', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(response('event: reset\ndata: {}\n\nevent: final\ndata: {"answer":"Да"}\n\nevent: token\ndata: "запоздалый"\n\n')));
    const receive = vi.fn(); await streamChat({}, new AbortController().signal, receive);
    expect(receive.mock.calls.map((call) => call[0].event)).toEqual(['reset', 'final']);
  });
  it('shows the stable public model error and passes cancellation signal to fetch', async () => {
    const fetcher = vi.fn().mockResolvedValue(response('event: error\ndata: {"status":503,"detail":"Модель не отвечает","code":"UPSTREAM_UNREACHABLE"}\n\n'));
    vi.stubGlobal('fetch', fetcher); const controller = new AbortController();
    await expect(streamChat({}, controller.signal, vi.fn())).rejects.toMatchObject({ message: 'Модель не отвечает', status: 503 });
    expect(fetcher.mock.calls[0][1].signal).toBe(controller.signal);
  });
  it('rejects an HTML startup/error page instead of showing it as an answer', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('<html>retry</html>', { headers: { 'Content-Type': 'text/html' } })));
    await expect(streamChat({}, new AbortController().signal, vi.fn())).rejects.toThrow('не прислал поток');
  });
});
