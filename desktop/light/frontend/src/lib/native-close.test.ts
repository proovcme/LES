import { afterEach, expect, it, vi } from 'vitest';
import { connectNativeClose } from './native-close';
import { ApiError } from './api';
import type { Workspace } from './workspace.svelte';

afterEach(() => vi.restoreAllMocks());
function fixture(prepareClose: () => Promise<void>) {
  const target = new EventTarget() as Window & { __TAURI__: { core: { invoke: (command: string, args?: Record<string, unknown>) => Promise<unknown> } } };
  target.__TAURI__ = { core: { invoke: vi.fn().mockResolvedValue(undefined) } };
  const workspace = { prepareClose, closing: false, error: '' } as unknown as Workspace;
  return { target, workspace };
}

it('acknowledges native close only after the full draft flush completes', async () => {
  let finish: () => void;
  const fixtureValue = fixture(vi.fn(() => new Promise<void>((resolve) => { finish = resolve; })));
  const { workspace, target } = fixtureValue;
  const bridge = connectNativeClose(workspace, target); await bridge.ready;
  target.dispatchEvent(new CustomEvent('les:close-request', { detail: { nonce: 3 } }));
  target.dispatchEvent(new CustomEvent('les:close-request', { detail: { nonce: 3 } }));
  expect(target.__TAURI__.core.invoke).toHaveBeenCalledTimes(1); // ready only, no premature acknowledgement
  expect(workspace.prepareClose).toHaveBeenCalledTimes(1);
  finish!(); await vi.waitFor(() => expect(target.__TAURI__.core.invoke).toHaveBeenCalledWith('editor_close_result', { nonce: 3, saved: true }));
  bridge.dispose();
});

it('keeps the window and editor open after a save failure', async () => {
  const { workspace, target } = fixture(vi.fn().mockRejectedValue(new ApiError('Конфликт черновиков', 409)));
  const bridge = connectNativeClose(workspace, target); await bridge.ready;
  target.dispatchEvent(new CustomEvent('les:close-request', { detail: { nonce: 5 } }));
  await vi.waitFor(() => expect(target.__TAURI__.core.invoke).toHaveBeenCalledWith('editor_close_result', { nonce: 5, saved: false }));
  expect(workspace.error).toContain('Окно остаётся открытым'); expect(workspace.closing).toBe(false);
  expect(target.__TAURI__.core.invoke).not.toHaveBeenCalledWith('editor_close_result', { nonce: 5, saved: true });
  bridge.dispose();
});

it('removes the native close listener on navigation and ignores malformed requests', async () => {
  const { workspace, target } = fixture(vi.fn().mockResolvedValue(undefined));
  const bridge = connectNativeClose(workspace, target); await bridge.ready;
  target.dispatchEvent(new CustomEvent('les:close-request', { detail: { nonce: 'foreign' } }));
  bridge.dispose(); target.dispatchEvent(new CustomEvent('les:close-request', { detail: { nonce: 7 } }));
  expect(workspace.prepareClose).not.toHaveBeenCalled();
});
