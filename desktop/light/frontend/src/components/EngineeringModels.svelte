<script lang="ts">
  import Modal from './Modal.svelte';
  import Icon from './Icon.svelte';
  import { ApiError, errorMessage } from '../lib/api';
  import UvyazkaInstall from './UvyazkaInstall.svelte';
  import { companionAddress, engineering, rememberCompanion } from '../lib/engineering';
  let { close, refresh }: { close: () => void; refresh: () => Promise<void> } = $props();
  type Snapshot = { dataset_id: string; source: { relative_path?: string; format?: string }; object_count: number;
    coverage: { status: string }; freshness: { status: string } };
  type RegisteredFile = { doc_id: string; relative_path: string; format: string; sha256?: string; available: boolean; missing: boolean };
  let url = $state(companionAddress()); let connected = $state('');
  let projects = $state<{ id: string; name: string }[]>([]); let project = $state('');
  let snapshots = $state<Snapshot[]>([]); let next = $state<number | null>(null);
  let files = $state<RegisteredFile[]>([]); let filesNext = $state<number | null>(null); let query = $state('');
  let view = $state<'files' | 'snapshots'>('files');
  const reads = new Map<string, string>();
  let busy = $state(''); let failure = $state(''); let reason = $state(''); let notice = $state('');
  function clearConnection() { connected = ''; projects = []; project = ''; snapshots = []; next = null; files = []; filesNext = null; notice = ''; failure = ''; reason = ''; }
  async function run(status: string, action: () => Promise<void>) {
    if (busy) return; busy = status; failure = ''; reason = ''; notice = '';
    try { await action(); } catch (e) { failure = errorMessage(e); reason = e instanceof ApiError ? e.code : ''; } finally { busy = ''; }
  }
  async function connect() {
    connected = ''; snapshots = []; project = ''; projects = [];
    const result = await engineering<{ projects: { id: string; name: string }[]; url: string }>(url, 'projects');
    connected = result.url; projects = result.projects; rememberCompanion(connected);
    notice = projects.length ? 'Выберите проект Увязки.' : 'В Увязке пока нет проектов.';
  }
  async function load(more = false) {
    if (!more) { snapshots = []; next = null; }
    const result = await engineering<{ items: Snapshot[]; next_offset: number | null }>(connected, 'datasets', { project_id: project, offset: more ? next : 0 });
    snapshots = more ? Array.from(new Map([...snapshots, ...result.items].map(item => [item.dataset_id, item])).values()) : result.items; next = result.next_offset;
    if (!snapshots.length) notice = 'Ранее прочитанных данных пока нет. На вкладке «Файлы» можно запустить чтение зарегистрированного DWG, RVT, IFC или другого поддерживаемого файла.';
  }
  async function add(item: Snapshot) {
    const result = await engineering<{ notice: string }>(connected, 'import', { project_id: project, dataset_id: item.dataset_id });
    await refresh(); notice = result.notice + ' Для вопросов выберите набор «Вложения чата» в области поиска.';
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
  async function selectProject() { files = []; filesNext = null; snapshots = []; next = null; if (project) await (view === 'files' ? loadFiles() : load()); }
  async function readFile(item: RegisteredFile) {
    const key = JSON.stringify([connected, project, item.doc_id, item.sha256]);
    const request_id = reads.get(key) ?? crypto.randomUUID(); reads.set(key, request_id);
    const result = await engineering<{ notice: string }>(connected, 'read_import', { project_id: project, doc_id: item.doc_id, request_id,
      ...(item.sha256 ? { expected_source_sha256: item.sha256 } : {}) });
    await refresh(); notice = result.notice + ' Для вопросов выберите набор «Вложения чата» в области поиска.';
  }
</script>
<Modal title="Чертежи и модели" {close} wide>
  <p class="muted">Увязка читает инженерные файлы. Лес ищет по сохранённым свойствам и открывает найденный объект с привязкой к редакции.</p>
  <form onsubmit={(event) => { event.preventDefault(); void run('Подключаю Увязку…', connect); }}>
    <label for="uvyazka-address">Локальный адрес Увязки</label>
    <input id="uvyazka-address" type="url" placeholder="http://127.0.0.1:8772/" bind:value={url} oninput={clearConnection} required disabled={!!busy} />
    <div class="modal-footer"><button disabled={!!busy || !url.trim()}><Icon name="external" />Подключить Увязку</button></div>
  </form>
  {#if connected}
    <label for="uvyazka-project">Проект</label>
    <select id="uvyazka-project" bind:value={project} disabled={!!busy} onchange={() => run('Читаю файлы проекта…', selectProject)}>
      <option value="">Выберите проект</option>{#each projects as item (item.id)}<option value={item.id}>{item.name}</option>{/each}
    </select>
    {#if project}<div class="source-actions" role="group" aria-label="Источники Увязки">
      <button class="quiet" aria-pressed={view === 'files'} disabled={!!busy} onclick={() => { view = 'files'; void run('Читаю файлы…', () => loadFiles()); }}>Файлы</button>
      <button class="quiet" aria-pressed={view === 'snapshots'} disabled={!!busy} onclick={() => { view = 'snapshots'; void run('Читаю снимки…', () => load()); }}>Ранее прочитанное</button>
      <a class="button quiet" href={`${connected}simulation/?project=${encodeURIComponent(project)}`} target="_blank" rel="noopener noreferrer">Открыть вьювер Увязки<Icon name="external" /></a></div>{/if}
    {#if project && view === 'files'}
      <form onsubmit={(event) => { event.preventDefault(); void run('Ищу файлы…', () => loadFiles()); }}>
        <label for="uvyazka-file-query">Имя файла или часть пути</label><input id="uvyazka-file-query" type="search" bind:value={query} maxlength="128" disabled={!!busy} />
        <div class="modal-footer"><button class="quiet" disabled={!!busy}>Найти / обновить файлы</button></div>
      </form>
      <ul class="session-list">{#each files as item (item.doc_id)}<li><strong>{item.relative_path}</strong>
        <p class="muted">{item.format.toUpperCase()} · {item.missing ? 'Исходник отсутствует' : item.available ? 'Прочитаем через Увязку, проверим редакцию и добавим данные в поиск.' : 'Читатель этого формата не установлен в Увязке.'}</p>
        <button class="quiet" disabled={!!busy || !item.available || item.missing} onclick={() => run('Увязка читает файл; затем проверю данные и добавлю в очередь…', () => readFile(item))}>Прочитать и добавить в поиск</button>
      </li>{/each}</ul>
      {#if filesNext !== null}<button class="quiet" disabled={!!busy} onclick={() => run('Читаю следующие файлы…', () => loadFiles(true))}>Ещё файлы</button>{/if}
    {:else if project}
    <ul class="session-list">
      {#each snapshots as item (item.dataset_id)}
        <li><strong>{item.source.relative_path || 'Сохранённый файл'}</strong>
          <p class="muted">{item.source.format?.toUpperCase() || 'Модель'} · объектов в снимке: {item.object_count} · {item.coverage.status === 'complete' ? 'Полнота заявлена читателем' : 'Прочитана часть содержимого'}</p>
          <p class="muted">{item.freshness.status === 'snapshot_metadata_matches' ? 'Редакция будет проверена по содержимому перед импортом.' : 'Для добавления требуется доступный оригинал той же редакции.'}</p>
          <button class="quiet" disabled={!!busy || !item.object_count} onclick={() => run('Проверяю исходник и читаю объекты…', () => add(item))}>Добавить снимок в поиск</button>
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
  <p class="muted">Снимок хранит данные и ограничения читателя, а не полный чертёж. Геометрия и подсветка доступны, когда их прочитала Увязка. Изменённая редакция требует нового снимка; автоматическая синхронизация пока не включена.</p>
  <div class="modal-footer"><button class="quiet" onclick={close}><Icon name="back" />Вернуться в чат</button></div>
</Modal>
