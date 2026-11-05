import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import AgentMemory from './AgentMemory.svelte';
import { Workspace } from '../lib/workspace.svelte';
let component: ReturnType<typeof mount> & { flush(): Promise<void> };
let saved: string; let revision: string; let fail: boolean; let finish: (() => void) | undefined;
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { 'Content-Type': 'application/json' } });
async function settle(predicate: () => boolean) { await vi.waitFor(() => { flushSync(); expect(predicate()).toBe(true); }); }
beforeEach(() => {
  saved = 'Исходное 日本語'; revision = 'r0'; fail = false; finish = undefined;
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; this.dispatchEvent(new Event('close')); };
  vi.stubGlobal('fetch', vi.fn(async (path: string, options?: RequestInit) => {
    if (path === '/api/projects') return json({ projects: [] });
    if (path.includes('action=policy')) return json({ value: { auto_memory: true, workspace_write: true }, revision: 'missing' });
    if (options?.method === 'POST') {
      const body = JSON.parse(String(options.body));
      if (fail) return json({ detail: 'Файл изменён в другом окне' }, 409);
      await new Promise<void>(resolve => { finish = resolve; });
      saved = body.text; revision = 'r1'; return json({ revision, persisted: true });
    }
    return json({ area: 'user', project_id: 0, path: 'USER.md', text: saved, revision, exists: true });
  }));
  component = mount(AgentMemory, { target: document.body, props: { open: true } }) as typeof component;
});
afterEach(async () => { await unmount(component); document.body.innerHTML = ''; vi.unstubAllGlobals(); });
function enter(value: string) { const textarea = document.querySelector<HTMLTextAreaElement>('#agent-memory-text')!; textarea.value = value; textarea.dispatchEvent(new Event('input', { bubbles: true })); flushSync(); }

it('waits for saving memory before closing the editor', async () => {
  await settle(() => !!document.querySelector('#agent-memory-text'));
  enter('Новая память 🌲');
  document.querySelector<HTMLButtonElement>('button[aria-label="Закрыть и вернуться"]')!.click();
  await settle(() => !!finish);
  expect(document.querySelector<HTMLDialogElement>('dialog')!.open).toBe(true);
  expect(saved).toBe('Исходное 日本語');
  finish!(); await settle(() => !document.querySelector('dialog'));
  expect(saved).toBe('Новая память 🌲');
});

it('keeps local text and shows the external version after a CAS conflict', async () => {
  await settle(() => !!document.querySelector('#agent-memory-text'));
  enter('Мои правки'); fail = true; saved = 'Правки другого окна'; revision = 'remote';
  await expect(component.flush()).rejects.toThrow('Файл изменён');
  await settle(() => !!document.querySelector('#remote-memory'));
  expect(document.querySelector<HTMLTextAreaElement>('#agent-memory-text')!.value).toBe('Мои правки');
  expect(document.querySelector<HTMLTextAreaElement>('#remote-memory')!.value).toBe('Правки другого окна');
  expect(document.querySelector<HTMLDialogElement>('dialog')!.open).toBe(true);
});

it('native close waits for the extra memory owner and retains the window on failure', async () => {
  const workspace = new Workspace(); workspace.beforeClose = () => Promise.reject(new Error('Memory not saved'));
  await expect(workspace.prepareClose()).rejects.toThrow('Memory not saved');
  expect(workspace.closing).toBe(false);
});

it('shows the selected ordinary scope instead of a blank select', async () => {
  await settle(() => !!document.querySelector('#agent-memory-text'));
  [...document.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.trim() === 'Память')!.click();
  await settle(() => !!document.querySelector('#memory-project'));
  const select = document.querySelector<HTMLSelectElement>('#memory-project')!;
  expect(select.value).toBe('0'); expect(select.selectedOptions[0].textContent).toBe('Общая');
});

it('explains recoverable deletion and keeps the removed file accessible through history', async () => {
  await settle(() => !!document.querySelector('#agent-memory-text'));
  let exists = true; const writes: object[] = [];
  vi.stubGlobal('fetch', vi.fn(async (path: string, options?: RequestInit) => {
    if (options?.method === 'POST') {
      const body = JSON.parse(String(options.body)); writes.push(body);
      if (body.action === 'delete') exists = false;
      return json({ revision: 'missing', persisted: true });
    }
    if (path.includes('action=list')) return json({ files: [{ path: 'notes/план.md', bytes: exists ? 10 : 0, exists }] });
    return json({ area: 'files', project_id: 0, path: 'workspaces/0/notes/план.md', text: exists ? 'План 🌲' : '', revision: exists ? 'file-revision' : 'missing', exists });
  }));
  [...document.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.trim() === 'Рабочие файлы')!.click();
  await settle(() => !!document.querySelector('#workfile-list'));
  [...document.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.trim() === 'Убрать файл')!.click();
  await settle(() => !!document.querySelector('[aria-label="Удаление рабочего файла"]'));
  expect(writes).toHaveLength(0);
  expect(document.querySelector('[aria-label="Удаление рабочего файла"]')?.textContent).toContain('можно вернуть');
  [...document.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.trim() === 'Убрать в историю')!.click();
  await settle(() => document.querySelector('#workfile-list option')?.textContent?.includes('в истории') === true);
  expect(writes).toEqual([{ action: 'delete', area: 'files', project_id: 0, path: 'notes/план.md', expected_revision: 'file-revision', reason: 'Удаление рабочего файла через редактор' }]);
  expect(document.querySelector<HTMLTextAreaElement>('#agent-memory-text')!.readOnly).toBe(true);
  expect([...document.querySelectorAll('button')].some(button => button.textContent?.trim() === 'История изменений')).toBe(true);
});

it('waits for a pending file operation before closing and prevents a second mutation', async () => {
  await settle(() => !!document.querySelector('#agent-memory-text'));
  let exists = true; let mutations = 0;
  vi.stubGlobal('fetch', vi.fn(async (path: string, options?: RequestInit) => {
    if (options?.method === 'POST') {
      mutations++;
      await new Promise<void>(resolve => { finish = () => { exists = false; resolve(); }; });
      return json({ revision: 'missing', persisted: true });
    }
    if (path.includes('action=list')) return json({ files: [{ path: 'one.md', bytes: 5, exists }] });
    return json({ area: 'files', project_id: 0, path: 'workspaces/0/one.md', text: exists ? 'Текст' : '', revision: exists ? 'one' : 'missing', exists });
  }));
  [...document.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.trim() === 'Рабочие файлы')!.click();
  await settle(() => !!document.querySelector('#workfile-list'));
  [...document.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.trim() === 'Убрать файл')!.click();
  await settle(() => !!document.querySelector('[aria-label="Удаление рабочего файла"]'));
  const remove = [...document.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.trim() === 'Убрать в историю')!;
  remove.click(); await settle(() => !!finish);
  expect(remove.disabled).toBe(true); expect(document.querySelector<HTMLSelectElement>('#workfile-list')!.disabled).toBe(true);
  document.querySelector<HTMLButtonElement>('[aria-label="Закрыть и вернуться"]')!.click();
  expect(document.querySelector<HTMLDialogElement>('dialog')!.open).toBe(true);
  remove.click(); expect(mutations).toBe(1);
  finish!(); await settle(() => !document.querySelector('dialog'));
});
