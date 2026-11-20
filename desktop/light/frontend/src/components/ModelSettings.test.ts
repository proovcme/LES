import { afterEach, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import ModelSettings from './ModelSettings.svelte';

let component: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (component) await unmount(component);
  document.body.replaceChildren();
  vi.unstubAllGlobals();
});

it.each([
  ['FreeToken', 'freetoken', 1919],
  ['llama.cpp', 'llama_cpp', 9931],
])('finds %s models and saves the connection before a role is assigned', async (label, extension, port) => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const route = '/api/model-connections';
  const model = 'local-model';
  const preset = { template_id: extension, base_url: `http://127.0.0.1:${port}/v1`, locality: 'loopback', extension_type: extension };
  const bindings = { answer: null, embeddings: null, local_fallback: null };
  let connection: Record<string, unknown> | undefined;
  let discovery: unknown;
  vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
    if (url === route + '/templates') return Response.json({ templates: [preset] });
    if (url === route + '/effective') return Response.json({ roles: {} });
    if (url === route + '/discover-models') {
      discovery = JSON.parse(init!.body as string);
      return Response.json({ status: 'ok', models: [model] });
    }
    if (url === route && init?.method === 'POST') {
      connection = { ...JSON.parse(init.body as string), connection_id: 'local', revision_id: 'v1', enabled: true };
      return Response.json(connection);
    }
    if (url === route) return Response.json({ connections: connection ? [connection] : [], bindings });
    throw new Error(`Unexpected request: ${url}`);
  }));
  const button = (label: string) => Array.from(document.querySelectorAll('button'))
    .find((item) => item.textContent?.trim() === label)!;
  component = mount(ModelSettings, { target: document.body, props: { close: vi.fn() } });
  await vi.waitFor(() => { flushSync(); expect(button(label).disabled).toBe(false); });
  button(label).click();
  await vi.waitFor(() => {
    flushSync();
    expect(document.querySelector<HTMLInputElement>('input[list="known-models"]')?.value).toBe(model);
  });
  expect(discovery).toEqual({ base_url: preset.base_url, locality: preset.locality });
  document.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
  await vi.waitFor(() => { flushSync(); expect(document.querySelector('li summary')?.textContent).toContain(`${model} · ${label}`); });
  expect(connection).toMatchObject({ model_id: model, extension_type: extension, locality: 'loopback' });
  expect(document.querySelector('dd')?.textContent).toBe('Не назначено');
});
