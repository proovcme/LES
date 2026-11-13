<script lang="ts">
  import { onMount } from 'svelte';
  import { errorMessage, request } from '../lib/api';

  type Folder = { id: string; root: string; dataset_name: string; enabled: number; status: string; message: string; missing: number };
  type FileState = { file_name: string; status: string };
  const endpoint = '/api/light/folder-watches';
  const folderStatus: Record<string, string> = {
    QUEUED: 'Ожидает проверки', AVAILABLE: 'Под наблюдением', PAUSED: 'На паузе',
    UNAVAILABLE: 'Папка недоступна', STALE: 'Проверка отложена', ERROR: 'Ошибка проверки',
  };
  const fileStatus: Record<string, string> = {
    MISSING: 'Файл отсутствует', STALE: 'Новая редакция', INDEXED: 'В поиске',
    PENDING: 'Ожидает индексации', ERROR: 'Ошибка индексации', SKIPPED: 'Пропущен',
  };
  let watches = $state<Folder[]>([]);
  let files = $state<FileState[]>([]);
  let selected = $state('');
  let offset = $state(0);
  let busy = $state('');
  let error = $state('');
  let requestVersion = 0;

  async function load() {
    const version = ++requestVersion;
    try {
      const list = await request<{ watches: Folder[] }>(endpoint);
      const detail = selected
        ? await request<{ files: FileState[] }>(endpoint + '?' + new URLSearchParams({ id: selected, offset: String(offset) }))
        : { files: [] };
      if (version === requestVersion) {
        watches = list.watches;
        files = detail.files;
        error = '';
      }
    } catch (failure) {
      if (version === requestVersion) error = errorMessage(failure);
    }
  }

  onMount(() => {
    void load();
    const timer = window.setInterval(() => { if (!busy) void load(); }, 5000);
    return () => { window.clearInterval(timer); requestVersion += 1; };
  });

  function open(id: string) {
    selected = selected === id ? '' : id;
    offset = 0;
    files = [];
    void load();
  }

  async function setEnabled(folder: Folder) {
    busy = folder.id;
    try {
      await request(endpoint, {
        method: 'POST',
        body: JSON.stringify({ id: folder.id, enabled: !folder.enabled }),
      });
      await load();
    } catch (failure) {
      error = errorMessage(failure);
    } finally {
      busy = '';
    }
  }
</script>

{#if watches.length}
  <section class="watches" aria-label="Наблюдаемые папки">
    <h3>Автообновление</h3>
    <ul>
      {#each watches as folder (folder.id)}
        <li>
          <div class="row">
            <div>
              <strong>{folder.root.split(/[\\/]/).filter(Boolean).at(-1) ?? folder.dataset_name}</strong>
              <span>{folder.dataset_name} · {folderStatus[folder.status] ?? 'Состояние неизвестно'}</span>
            </div>
            <button class="quiet" type="button" aria-expanded={selected === folder.id} onclick={() => open(folder.id)}>
              {selected === folder.id ? 'Свернуть' : 'Подробнее'}
            </button>
          </div>
          {#if folder.missing}<p>Отсутствуют: {folder.missing}</p>{/if}
          {#if folder.message && folder.status !== 'PAUSED'}<p>{folder.message}</p>{/if}
          {#if selected === folder.id}
            <div class="details">
              <p>{folder.root}</p>
              {#if files.length}
                <ul class="files">
                  {#each files as file (file.file_name)}
                    <li><span>{file.file_name}</span><span>{fileStatus[file.status] ?? file.status}</span></li>
                  {/each}
                </ul>
              {/if}
              <div class="controls">
                <button class="quiet" type="button" disabled={!!busy} onclick={() => setEnabled(folder)}>
                  {folder.enabled ? 'Приостановить' : 'Возобновить'}
                </button>
                {#if offset > 0}<button class="quiet" type="button" onclick={() => { offset -= 100; void load(); }}>Предыдущие</button>{/if}
                {#if files.length === 100}<button class="quiet" type="button" onclick={() => { offset += 100; void load(); }}>Следующие</button>{/if}
              </div>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  </section>
{/if}
{#if error}<p class="watch-error" role="alert">Не удалось загрузить наблюдение за папками. {error}</p>{/if}

<style>
  .watches { margin-top: 24px; }
  h3 { font-size: 18px; margin: 0 0 8px; }
  ul { list-style: none; margin: 0; padding: 0; }
  .watches > ul > li { border-top: 1px solid var(--forest-line); padding: 9px 0; }
  .row, .files li, .controls { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: center; gap: 8px; }
  .row strong, .row span { display: block; overflow-wrap: anywhere; }
  p, .row span, .files li span:last-child { margin: 3px 0; color: var(--forest-muted); font-size: 14px; overflow-wrap: anywhere; }
  .files { max-height: 35vh; overflow: auto; }
  .files li { padding: 4px 0; gap: 4px 8px; font-size: 14px; overflow-wrap: anywhere; }
  .details { margin: 8px 0 0; padding: 8px 0 0; border-top: 1px solid var(--forest-line); }
  .controls { justify-content: flex-start; margin-top: 8px; }
  .watch-error { color: var(--forest-error, #8a211a); }
</style>
