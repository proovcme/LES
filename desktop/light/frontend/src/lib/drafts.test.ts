import { afterEach, expect, it, vi } from 'vitest';
import { DraftWriter } from './drafts';
afterEach(() => vi.unstubAllGlobals());
const initial = { revision: 0, text: '', attachment: null };
const json = (value: object, status = 200) => new Response(JSON.stringify(value), { status });

it('serializes saves with fresh revisions, preserving the latest text', async () => {
  const writes: { revision: number; text: string }[] = [];
  vi.stubGlobal('fetch', vi.fn(async (_path, init) => {
    const body = JSON.parse(init.body); writes.push(body);
    await new Promise((resolve) => setTimeout(resolve, 2));
    return json({ ...body, revision: body.revision + 1 });
  }));
  const writer = new DraftWriter('session', initial);
  await Promise.all([writer.save('один', null), writer.save('два', null), writer.save('три', null)]);
  expect(writes.map(({ revision, text }) => [revision, text])).toEqual([[0, 'один'], [1, 'два'], [2, 'три']]);
});

it('recognizes a committed save after a lost response', async () => {
  const fetcher = vi.fn().mockRejectedValueOnce(new TypeError('network')).mockResolvedValueOnce(json({ revision: 1, text: 'Сохранено', attachment: null }))
    .mockResolvedValueOnce(json({ revision: 2, text: 'Следующий', attachment: null }));
  vi.stubGlobal('fetch', fetcher);
  const writer = new DraftWriter('session', initial);
  await writer.save('Сохранено', null); await writer.save('Следующий', null);
  expect(JSON.parse(fetcher.mock.calls[2][1].body).revision).toBe(1);
});

it('never overwrites a conflicting draft in another window', async () => {
  const fetcher = vi.fn().mockResolvedValueOnce(json({ detail: 'Другой черновик' }, 409))
    .mockResolvedValueOnce(json({ revision: 1, text: 'Другой текст', attachment: null }));
  vi.stubGlobal('fetch', fetcher); const writer = new DraftWriter('session', initial);
  await expect(writer.save('Мой текст', null)).rejects.toMatchObject({ status: 409 });
  await expect(writer.save('Ещё мой текст', null)).rejects.toMatchObject({ status: 409 });
  expect(fetcher).toHaveBeenCalledTimes(2);
});
