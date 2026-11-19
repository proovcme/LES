<script lang="ts">
  import { onMount } from 'svelte';
  import Modal from './Modal.svelte';
  import Icon from './Icon.svelte';
  import { ApiError, errorMessage } from '../lib/api';
  import UvyazkaInstall from './UvyazkaInstall.svelte';
  import { companionAddress, engineering, rememberCompanion, savedCompanion, syncCompanion, type CompanionProject } from '../lib/engineering';
  let { close, refresh, currentProject, useProject }: {
    close: () => void; refresh: () => Promise<void>; currentProject?: number; useProject?: (id: number) => Promise<void>;
  } = $props();
  type Snapshot = { dataset_id: string; source: { relative_path?: string; format?: string }; object_count: number;
    coverage: { status: string }; freshness: { status: string } };
  type RegisteredFile = { doc_id: string; relative_path: string; format: string; sha256?: string; available: boolean; missing: boolean };
  let url = $state(companionAddress()); let connected = $state('');
  let projects = $state<CompanionProject[]>([]); let project = $state('');
  const localProject = $derived(projects.find(item => item.id === project)?.local_id);
  let snapshots = $state<Snapshot[]>([]); let next = $state<number | null>(null);
  let files = $state<RegisteredFile[]>([]); let filesNext = $state<number | null>(null); let query = $state('');
  let view = $state<'files' | 'snapshots'>('files');
  let preview = $state<{ id: string; url: string }>();
  const reads = new Map<string, string>();
  let busy = $state(''); let failure = $state(''); let reason = $state(''); let notice = $state('');
  function clearConnection() { connected = ''; projects = []; project = ''; snapshots = []; next = null; files = []; filesNext = null; preview = undefined; notice = ''; failure = ''; reason = ''; }
  async function run(status: string, action: () => Promise<void>) {
    if (busy) return; busy = status; failure = ''; reason = ''; notice = '';
    try { await action(); } catch (e) { failure = errorMessage(e); reason = e instanceof ApiError ? e.code : ''; } finally { busy = ''; }
  }
  async function connect(address: string) {
    const result = await syncCompanion(address);
    if (!result.url) return;
    url = connected = result.url; projects = result.projects; rememberCompanion(connected);
    const available = projects.filter(item => item.available);
    project = available.find(item => item.id === project)?.id
      ?? available.find(item => item.local_id === currentProject)?.id
      ?? available.find(item => item.id === result.default_project)?.id ?? available[0]?.id ?? '';
    await selectProject();
    if (!notice) notice = projects.length ? 'Проекты Увязки обновлены в Лесе.' : 'В Увязке пока нет проектов.';
  }
  onMount(() => {
    void run('Проверяю подключение к Увязке…', async () => {
      const saved = await engineering<{ url: string | null }>('', 'connection');
      if (saved.url) {
        url = saved.url;
        await connect('');
      } else if (savedCompanion()) {
        await connect(url);
      }
    });
  });
  async function load(more = false) {
    if (!more) { snapshots = []; next = null; }
    const result = await engineering<{ items: Snapshot[]; next_offset: number | null }>(connected, 'datasets', { project_id: project, offset: more ? next : 0 });
    snapshots = more ? Array.from(new Map([...snapshots, ...result.items].map(item => [item.dataset_id, item])).values()) : result.items; next = result.next_offset;
    if (!snapshots.length) notice = 'Ранее прочитанных данных пока нет. На вкладке «Файлы» можно запустить чтение зарегистрированного DWG, RVT, IFC или другого поддерживаемого файла.';
  }
  async function add(item: Snapshot) {
    const result = await engineering<{ notice: string }>(connected, 'import', { project_id: project, dataset_id: item.dataset_id });
    await refresh(); notice = result.notice;
  }
  async function loadFiles(more = false) {
    if (!more) { files = []; filesNext = null; }
    let offset = more ? filesNext : 0;
    // Non-model files share the upstream inventory. Empty pages can still continue.
    for (let pages = 0; pages < 10; pages++) {
      const result = await engineering<{ items: RegisteredFile[]; next_offset: number | null }>(connected, 'sources', { project_id: project, offset, q: query });
      files = Array.from(new Map([...files, ...result.items].map(item => [item.doc_id, item])).values()); filesNext = result.next_offset;
      if (result.items.length || filesNext === null) break;
      if (filesNext <= (offset ?? 0)) throw new Error('Invalid file continuation');
      offset = filesNext;
    }
    if (!files.length) notice = filesNext === null ? 'Поддерживаемые инженерные файлы не найдены. Добавьте исходники в проект Увязки или измените поиск.' : 'В этой части каталога инженерных файлов нет. Продолжите список или уточните имя.';
  }
  async function selectProject() { files = []; filesNext = null; snapshots = []; next = null; preview = undefined; if (project) await (view === 'files' ? loadFiles() : load()); }
  function readArgs(item: RegisteredFile) {
    const key = JSON.stringify([connected, project, item.doc_id, item.sha256]);
    const request_id = reads.get(key) ?? crypto.randomUUID(); reads.set(key, request_id);
    return { project_id: project, doc_id: item.doc_id, request_id,
      ...(item.sha256 ? { expected_source_sha256: item.sha256 } : {}) };
  }
  async function readFile(item: RegisteredFile) {
    const result = await engineering<{ notice: string }>(connected, 'read_import', readArgs(item));
    await refresh(); notice = result.notice;
  }
  async function preparePreview(id: string, args: object) {
    preview = undefined;
    const result = await engineering<{ url: string }>(connected, 'preview', args);
    preview = { id, url: result.url };
  }
</script>
<Modal title="Чертежи и модели" {close} wide>
  <p class="muted">Увязка читает инженерные файлы. Лес ищет по сохранённым свойствам и открывает найденный объект с привязкой к редакции.</p>
  <form onsubmit={(event) => { event.preventDefault(); void run('Обновляю проекты Увязки…', () => connect(url)); }}>
    <label for="uvyazka-address">Локальный адрес Увязки</label>
    <input id="uvyazka-address" type="url" bind:value={url} oninput={clearConnection} required disabled={!!busy} />
    <div class="modal-footer"><button disabled={!!busy || !url.trim()}><Icon name="external" />{connected ? 'Обновить проекты' : 'Подключить Увязку'}</button></div>
  </form>
  {#if connected}
    {#if projects.length > 1}
      <label for="uvyazka-project">Файлы проекта</label>
      <select id="uvyazka-project" bind:value={project} disabled={!!busy || !project} onchange={() => run('Читаю файлы проекта…', selectProject)}>
        {#each projects as item (item.id)}<option value={item.id} disabled={!item.available}>{item.name}{item.available ? '' : ' · недоступен'}</option>{/each}
      </select>
    {:else if project}
      <h3>{projects[0].name}</h3>
    {/if}
    {#if projects.length && !project}<p class="muted">Увязка сейчас не может открыть файлы этих проектов.</p>{/if}
    {#if project}<div class="source-actions" role="group" aria-label="Источники Увязки">
      <button class="quiet" aria-pressed={view === 'files'} disabled={!!busy} onclick={() => { view = 'files'; void run('Читаю файлы…', () => loadFiles()); }}>Файлы</button>
      <button class="quiet" aria-pressed={view === 'snapshots'} disabled={!!busy} onclick={() => { view = 'snapshots'; void run('Читаю снимки…', () => load()); }}>Ранее прочитанное</button>
      {#if useProject && localProject}
        <button class="quiet" disabled={!!busy} onclick={() => useProject?.(localProject)}>Открыть проект в чате</button>
      {/if}
      <a class="button quiet" href={`${connected}simulation/?project=${encodeURIComponent(project)}`} target="_blank" rel="noopener noreferrer">Открыть вьювер Увязки<Icon name="external" /></a></div>{/if}
    {#if project && view === 'files'}
      <form onsubmit={(event) => { event.preventDefault(); void run('Ищу файлы…', () => loadFiles()); }}>
        <label for="uvyazka-file-query">Имя файла или часть пути</label><input id="uvyazka-file-query" type="search" bind:value={query} maxlength="128" disabled={!!busy} />
        <div class="modal-footer"><button class="quiet" disabled={!!busy}>Найти / обновить файлы</button></div>
      </form>
      <ul class="session-list">{#each files as item (item.doc_id)}<li><strong>{item.relative_path}</strong>
        <p class="muted">{item.format.toUpperCase()} · {item.missing ? 'Исходник отсутствует' : item.available ? 'Прочитаем через Увязку, проверим редакцию и добавим данные в поиск.' : 'Читатель этого формата не установлен в Увязке.'}</p>
        <button class="quiet" disabled={!!busy || !item.available || item.missing} onclick={() => run('Увязка читает файл; затем проверю данные и добавлю в очередь…', () => readFile(item))}>Прочитать и добавить в поиск</button>
        {#if preview?.id === item.doc_id}
          <a class="button quiet" href={preview.url} target="_blank" rel="noopener noreferrer">Открыть файл в Увязке<Icon name="external" /></a>
        {:else}
          <button class="quiet" disabled={!!busy || !item.available || item.missing} onclick={() => run('Подготавливаю просмотр файла…', () => preparePreview(item.doc_id, readArgs(item)))}>Просмотр файла</button>
        {/if}
      </li>{/each}</ul>
      {#if filesNext !== null}<button class="quiet" disabled={!!busy} onclick={() => run('Читаю следующие файлы…', () => loadFiles(true))}>Ещё файлы</button>{/if}
    {:else if project}
    <ul class="session-list">
      {#each snapshots as item (item.dataset_id)}
        <li><strong>{item.source.relative_path || 'Сохранённый файл'}</strong>
          <p class="muted">{item.source.format?.toUpperCase() || 'Модель'} · объектов в снимке: {item.object_count} · {item.coverage.status === 'complete' ? 'Полнота заявлена читателем' : 'Прочитана часть содержимого'}</p>
          <p class="muted">{item.freshness.status === 'snapshot_metadata_matches' ? 'Редакция будет проверена по содержимому перед импортом.' : 'Для добавления требуется доступный оригинал той же редакции.'}</p>
          <button class="quiet" disabled={!!busy || !item.object_count} onclick={() => run('Проверяю исходник и читаю объекты…', () => add(item))}>Добавить снимок в поиск</button>
          {#if preview?.id === item.dataset_id}
            <a class="button quiet" href={preview.url} target="_blank" rel="noopener noreferrer">Открыть файл в Увязке<Icon name="external" /></a>
          {:else}
            <button class="quiet" disabled={!!busy} onclick={() => run('Проверяю редакцию файла…', () => preparePreview(item.dataset_id, { project_id: project, dataset_id: item.dataset_id }))}>Просмотр файла</button>
          {/if}
        </li>
      {/each}
    </ul>
    {#if next !== null}<button class="quiet" disabled={!!busy} onclick={() => run('Читаю следующие снимки…', () => load(true))}>Ещё снимки</button>{/if}
    {/if}
  {/if}
  {#if busy}<p role="status" aria-live="polite">{busy}</p><progress aria-label={busy}></progress>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if failure}<p class="error" role="alert">{failure}</p>{/if}
  {#if !connected || reason === 'ENGINEERING_UNAVAILABLE' || reason === 'ENGINEERING_VIEWER_UPDATE_REQUIRED'}<UvyazkaInstall {reason} />{/if}
  <p class="muted">Список проектов берётся из Увязки. Просмотр открывается в Увязке. Для поиска по содержимому сначала добавьте файл в индекс Леса.</p>
  <div class="modal-footer"><button class="quiet" onclick={close}><Icon name="back" />Вернуться в чат</button></div>
</Modal>
