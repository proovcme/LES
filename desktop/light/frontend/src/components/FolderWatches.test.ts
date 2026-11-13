import { afterEach, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import FolderWatches from './FolderWatches.svelte';

let component: ReturnType<typeof mount> | undefined;
const endpoint = '/api/light/folder-watches';
const response = (value: unknown) => new Response(JSON.stringify(value));
const button = (name: string) => [...document.querySelectorAll('button')]
  .find(item => item.textContent?.trim() === name);

afterEach(async () => {
  if (component) await unmount(component);
  document.body.replaceChildren();
  vi.unstubAllGlobals();
});

it('shows folder availability and missing files separately and can pause a watch', async () => {
  let enabled = 1;
  const writes: unknown[] = [];
  vi.stubGlobal('fetch', vi.fn(async (path: string, options?: RequestInit) => {
    if (path === endpoint && options?.method === 'POST') {
      writes.push(JSON.parse(String(options.body)));
      enabled = 0;
      return response({ id: 'watch-one', enabled: false });
    }
    if (path.startsWith(endpoint + '?')) return response({ files: [
      { file_name: 'Рабочая/План.pdf', status: 'INDEXED' },
      { file_name: 'Рабочая/Старый лист.pdf', status: 'MISSING' },
    ] });
    return response({ watches: [{
      id: 'watch-one', dataset_name: 'Документы', root: 'Рабочая', enabled,
      status: enabled ? 'UNAVAILABLE' : 'PAUSED', message: '', missing: 1,
    }] });
  }));
  component = mount(FolderWatches, { target: document.body });

  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Папка недоступна'); });
  button('Подробнее')!.click();
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('Файл отсутствует'); });
  expect(document.body.textContent).toContain('В поиске');

  button('Приостановить')!.click();
  await vi.waitFor(() => { flushSync(); expect(document.body.textContent).toContain('На паузе'); });
  expect(writes).toEqual([{ id: 'watch-one', enabled: false }]);
});
