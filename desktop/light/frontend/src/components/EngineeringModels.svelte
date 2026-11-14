<script lang="ts">
  import Modal from './Modal.svelte';
  import Icon from './Icon.svelte';
  import { ApiError, errorMessage } from '../lib/api';
  import UvyazkaInstall from './UvyazkaInstall.svelte';
  import { companionAddress, engineering, rememberCompanion } from '../lib/engineering';
  let { close, refresh }: { close: () => void; refresh: () => Promise<void> } = $props();
  type Snapshot = { dataset_id: string; source: { relative_path?: string; format?: string }; object_count: number;
    coverage: { status: string }; freshness: { status: string } };
  let url = $state(companionAddress()); let connected = $state('');
  let projects = $state<{ id: string; name: string }[]>([]); let project = $state('');
  let snapshots = $state<Snapshot[]>([]); let next = $state<number | null>(null);
  let busy = $state(''); let failure = $state(''); let reason = $state(''); let notice = $state('');
  function clearConnection() { connected = ''; projects = []; project = ''; snapshots = []; next = null; notice = ''; failure = ''; reason = ''; }
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
    snapshots = more ? [...snapshots, ...result.items] : result.items; next = result.next_offset;
    if (!snapshots.length) notice = 'Сначала прочитайте зарегистрированный DWG, RVT, IFC или другой поддерживаемый файл в Увязке. Здесь появятся сохранённые снимки.';
  }
  async function add(item: Snapshot) {
    const result = await engineering<{ notice: string }>(connected, 'import', { project_id: project, dataset_id: item.dataset_id });
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
    <select id="uvyazka-project" bind:value={project} disabled={!!busy} onchange={() => { if (project) void run('Читаю список снимков…', () => load()); else { snapshots = []; next = null; notice = ''; } }}>
      <option value="">Выберите проект</option>{#each projects as item (item.id)}<option value={item.id}>{item.name}</option>{/each}
    </select>
    {#if project}<div class="modal-footer"><button class="quiet" disabled={!!busy} onclick={() => run('Обновляю снимки…', () => load())}>Обновить список</button>
      <a class="button quiet" href={`${connected}simulation/?project=${encodeURIComponent(project)}`} target="_blank" rel="noopener noreferrer">Открыть вьювер Увязки<Icon name="external" /></a></div>{/if}
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
  {#if busy}<p role="status" aria-live="polite">{busy}</p><progress aria-label={busy}></progress>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if failure}<p class="error" role="alert">{failure}</p>{/if}
  {#if !connected || reason === 'ENGINEERING_UNAVAILABLE' || reason === 'ENGINEERING_VIEWER_UPDATE_REQUIRED'}<UvyazkaInstall {reason} />{/if}
  <p class="muted">Снимок хранит данные и ограничения читателя, а не полный чертёж. Геометрия и подсветка доступны, когда их прочитала Увязка. Изменённая редакция требует нового снимка; автоматическая синхронизация пока не включена.</p>
  <div class="modal-footer"><button class="quiet" onclick={close}><Icon name="back" />Вернуться в чат</button></div>
</Modal>
