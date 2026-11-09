<script lang="ts">
  import Modal from './Modal.svelte';
  import { onMount } from 'svelte';
  import { errorMessage } from '../lib/api';

  type Release = { available: boolean; unpublished?: boolean; current_build: number;
    version?: string; build?: number; sha256?: string; bytes?: number };
  type Desktop = Window & { __TAURI__?: { core: { invoke: <T>(name: string, args?: object) => Promise<T> } } };
  let { close }: { close: () => void } = $props();
  const invoke = (window as Desktop).__TAURI__?.core.invoke;
  let release = $state<Release>();
  let progress = $state('');
  let error = $state('');
  let waiting = $state(false);
  async function run(message: string, action: () => Promise<void>) {
    if (progress || waiting) return;
    progress = message; error = '';
    try { await action(); }
    catch (e) { error = typeof e === 'string' ? e : errorMessage(e); }
    finally { progress = ''; }
  }
  function check() {
    return run('Проверяю опубликованный выпуск…', async () => {
      release = undefined;
      release = await invoke?.<Release>('update_check');
    });
  }
  function install() {
    const item = release;
    if (!item?.available || !item.build || !item.sha256) return;
    void run('Загружаю и проверяю установщик…', async () => {
      waiting = true;
      try { await invoke?.('update_install', { build: item.build, sha256: item.sha256 }); }
      catch (e) { waiting = false; throw e; }
    });
  }
  function closeFailed() {
    if (!waiting) return;
    waiting = false; error = 'Диалог пока не сохранён. Исправьте ошибку в чате и повторите установку.';
  }
  onMount(() => {
    window.addEventListener('les:close-failed', closeFailed);
    return () => window.removeEventListener('les:close-failed', closeFailed);
  });
</script>

<Modal title="Обновление Леса" {close} beforeClose={async () => { if (progress || waiting) throw new Error(); }}>
  {#if !invoke}<p>Проверка и установка обновлений доступны в окне приложения Windows.</p>{/if}
  {#if release}
    <p>Установлена сборка {release.current_build}.</p>
    {#if release.unpublished}<p>В канале пока нет установщика.</p>
    {:else if release.available}
      <p>Доступен Лес {release.version}, сборка {release.build}{release.bytes ? ` · ${Math.ceil(release.bytes / 1048576)} МБ` : ''}.</p>
      <p>После загрузки Лес сохранит диалог, закроется и откроет установщик. Если подключён внешний MCP-клиент, завершите его сеанс перед продолжением установки.</p>
    {:else}<p>В канале нет более новой сборки.</p>{/if}
  {/if}
  <p role="status">{progress || (waiting ? 'Сохраняю диалог и открываю установщик…' : '')}</p>
  {#if error}<p role="alert">{error}</p>{/if}
  <div class="modal-footer">
    <button class:quiet={release?.available} disabled={!invoke || !!progress || waiting} onclick={check}>Проверить обновления</button>
    {#if release?.available}<button disabled={!!progress || waiting} onclick={install}>Скачать и открыть установщик</button>{/if}
  </div>
</Modal>
