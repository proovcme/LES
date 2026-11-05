import { errorMessage } from './api';
import type { Workspace } from './workspace.svelte';

interface DesktopWindow extends Window {
  __TAURI__?: { core: { invoke: (command: string, args?: Record<string, unknown>) => Promise<unknown> } };
}

export function connectNativeClose(workspace: Workspace, target: DesktopWindow = window) {
  const invoke = target.__TAURI__?.core.invoke;
  let pending = false;
  const close = async (event: Event) => {
    const nonce: unknown = (event as CustomEvent).detail?.nonce;
    if (!invoke || pending || typeof nonce !== 'number' || !Number.isSafeInteger(nonce) || nonce < 1) return;
    pending = true;
    try {
      await workspace.prepareClose();
      await invoke('editor_close_result', { nonce, saved: true });
    } catch (error) {
      workspace.closing = false;
      workspace.error = `Окно остаётся открытым: изменения не удалось сохранить. ${errorMessage(error)}`;
      await invoke('editor_close_result', { nonce, saved: false }).catch(() => undefined);
    } finally { pending = false; }
  };
  target.addEventListener('les:close-request', close);
  return {
    ready: invoke ? invoke('editor_ready').then(() => undefined) : Promise.resolve(),
    dispose() { target.removeEventListener('les:close-request', close); },
  };
}
