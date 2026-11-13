<script lang="ts">
  import { onMount } from 'svelte';
  import { errorMessage, request } from '../lib/api';
  import type { Dataset } from '../lib/contracts';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';
  import FolderWatches from './FolderWatches.svelte';

  type Project = { id: number; name: string; status: string };
  type Desktop = Window & { __TAURI__?: { core: { invoke: (name: string, args?: object) => Promise<string | null> } } };
  let { datasets, refresh, useProject, close }: {
    datasets: Dataset[]; refresh: () => Promise<void>; useProject: (id: number) => Promise<void>; close: () => void;
  } = $props();
  let section = $state<'folders' | 'projects'>('folders');
  let projects = $state<Project[]>([]);
  let selected = $state('');
  let name = $state('');
  let path = $state('');
  let followFolder = $state(true);
  let watchRevision = $state(0);
  let project = $state<number | null>(null);
  let projectName = $state('');
  let busy = $state('');
  let notice = $state('');
  let error = $state('');
  const pick = (window as Desktop).__TAURI__?.core.invoke;
  const post = <T,>(url: string, body: object) => request<T>(url, { method: 'POST', body: JSON.stringify(body) });
  let suggestedName = $derived(path.trim().split(/[\\/]/).filter(Boolean).at(-1)?.slice(0, 120) ?? '');
  let targetName = $derived(datasets.find((item) => item.id === selected)?.name);
  let linkedProject = $derived(projects.find((item) => item.id === project)?.name);

  async function act(message: string, fn: () => Promise<void>) {
    if (busy) return;
    busy = message; notice = ''; error = '';
    try { await fn(); }
    catch (failure) { error = typeof failure === 'string' ? failure : errorMessage(failure); }
    finally { busy = ''; }
  }
  async function loadProjects() {
    projects = (await request<{ projects: Project[] }>('/api/projects')).projects.filter((entry) => entry.status !== 'archived');
  }
  onMount(() => { void act('Загружаем данные…', async () => { await Promise.all([loadProjects(), refresh()]); }); });

  async function choose() {
    if (!pick) return;
    const folder = await pick('pick_folder', { initial: path.trim() || null });
    if (folder) path = folder;
  }
  async function addFolder() {
    const root = path.trim();
    const label = name.trim() || suggestedName;
    if (!root || (!selected && !label)) return;
    if (!selected) {
      const dataset = await post<Dataset>('/api/rag/datasets', { name: label });
      selected = dataset.id;
      await refresh();
    }
    if (project !== null) {
      await post('/api/projects/' + project + '/links', { kind: 'dataset', ref: selected });
    }
    await post('/api/rag/index-external', { path: root, dataset_id: selected, background: true, parse_limit: 500, watch: followFolder });
    watchRevision += 1;
    path = ''; name = ''; selected = '';
    notice = 'Папка добавлена в очередь. Ход индексации — в разделе «Состояние».';
  }
  async function createProject() {
    const trimmed = projectName.trim();
    if (!trimmed) return;
    const item = await post<Project>('/api/projects', { name: trimmed });
    await loadProjects();
    project = item.id; projectName = '';
    notice = 'Проект создан. Теперь его можно открыть в чате или привязать к нему папку.';
  }
  function switchTo(next: typeof section) {
    section = next;
    notice = ''; error = '';
  }
</script>

<Modal title="Данные и проекты" {close} editor>
  <div class="section-switch" role="group" aria-label="Раздел данных">
    <button type="button" class="quiet" aria-pressed={section === 'folders'} disabled={!!busy} onclick={() => switchTo('folders')}><Icon name="folder" size={18} /> Папки</button>
    <button type="button" class="quiet" aria-pressed={section === 'projects'} disabled={!!busy} onclick={() => switchTo('projects')}><Icon name="file" size={18} /> Проекты</button>
  </div>

  {#if section === 'folders'}
    <form class="editor-form library-form" onsubmit={(event) => { event.preventDefault(); void act('Подключаем папку…', addFolder); }}>
      <div class="intro">
        <h3>Подключить папку</h3>
        <p>Лес прочитает документы и сделает их доступными в поиске. Исходные файлы останутся на месте.</p>
      </div>
      <div class="folder-choice">
        <label class="field">Путь к папке
          <input required bind:value={path} disabled={!!busy} spellcheck="false" placeholder="Вставьте путь к папке" />
        </label>
        {#if pick}<button type="button" class="quiet" disabled={!!busy} onclick={() => act('Выберите папку в Проводнике…', choose)}><Icon name="folder" size={18} /> Выбрать папку</button>{/if}
      </div>

      {#if !selected}
        <label class="field">Название набора
          <input maxlength="120" bind:value={name} disabled={!!busy} placeholder={suggestedName || 'По умолчанию — имя папки'} />
        </label>
      {/if}
      {#if datasets.length || projects.length || selected !== ''}
        <details class="destinations">
          <summary>Куда добавить{#if targetName || linkedProject}<span>{[targetName, linkedProject].filter(Boolean).join(' · ')}</span>{/if}</summary>
          <div class="destination-fields">
            {#if datasets.length || selected !== ''}
              <label class="field">Набор документов
                <select bind:value={selected} disabled={!!busy}>
                  <option value="">Создать новый набор</option>
                  {#each datasets as dataset (dataset.id)}<option value={dataset.id}>{dataset.name}</option>{/each}
                  {#if selected !== '' && !targetName}<option value={selected}>Набор для этой папки</option>{/if}
                </select>
              </label>
            {/if}
            {#if projects.length}
              <label class="field">Связать с проектом
                <select bind:value={project} disabled={!!busy}>
                  <option value={null}>Без проекта</option>
                  {#each projects as item (item.id)}<option value={item.id}>{item.name}</option>{/each}
                </select>
              </label>
            {/if}
          </div>
        </details>
      {/if}
      <label class="follow-folder"><input type="checkbox" bind:checked={followFolder} disabled={!!busy} /> Обновлять автоматически</label>
      <div class="actions"><button disabled={!!busy || !path.trim() || (!selected && !name.trim() && !suggestedName)}>Добавить папку</button></div>
    </form>
    {#key watchRevision}<FolderWatches />{/key}
  {:else}
    <div class="intro">
      <h3>Проекты</h3>
      <p>Соберите связанные документы и чаты в одном месте.</p>
    </div>
    {#if projects.length}
      <ul class="project-list">
        {#each projects as item (item.id)}
          <li><span>{item.name}</span><button class="quiet" disabled={!!busy} onclick={() => act('Открываем проект…', async () => { await useProject(item.id); close(); })}>Открыть в чате</button></li>
        {/each}
      </ul>
    {/if}
    <form class="editor-form library-form" onsubmit={(event) => { event.preventDefault(); void act('Создаём проект…', createProject); }}>
      <label class="field">Название нового проекта
        <input required maxlength="160" bind:value={projectName} disabled={!!busy} placeholder="Например, рабочая документация" />
      </label>
      <div class="actions"><button disabled={!!busy || !projectName.trim()}>Создать проект</button></div>
    </form>
  {/if}

  <div role="status" class:message={busy || notice}>{busy || notice}</div>
  {#if error}<p class="message error" role="alert">{error}</p>{/if}
</Modal>

<style>
  .section-switch { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 20px; }
  .section-switch button, .folder-choice button { display: inline-flex; align-items: center; justify-content: center; gap: 8px; }
  .section-switch button[aria-pressed="true"] { background: var(--forest-floor); border-color: var(--forest-line); font-weight: 700; }
  .intro h3 { margin: 0 0 5px; font-size: 18px; }
  .intro p { margin: 0; color: var(--forest-muted); line-height: 1.55; font-size: 14px; }
  .library-form { display: grid; gap: 16px; }
  .intro + .library-form { margin-top: 16px; }
  .folder-choice { display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: end; gap: 8px; }
  .folder-choice button { min-height: 44px; }
  .destinations { border: 1px solid var(--forest-line); border-radius: 10px; background: var(--forest-floor); }
  .destinations summary { padding: 12px; cursor: pointer; font-size: 14px; font-weight: 600; }
  .destinations summary span { display: inline; margin-left: 8px; font-weight: 400; overflow-wrap: anywhere; color: var(--forest-muted); }
  .destination-fields { display: grid; gap: 12px; grid-template-columns: repeat(auto-fit, minmax(min(100%, 200px), 1fr)); padding: 0 12px 12px; }
  .actions { display: flex; justify-content: flex-end; }
  .follow-folder { display: flex; align-items: center; gap: 8px; min-height: 32px; }
  .follow-folder input { width: 18px; height: 18px; accent-color: var(--forest-accent); }
  .project-list { display: grid; gap: 6px; max-height: 38vh; overflow-y: auto; list-style: none; padding: 0; margin: 16px 0; }
  .project-list li { display: flex; justify-content: space-between; align-items: center; gap: 12px; padding: 6px 8px 6px 12px; border: 1px solid var(--forest-line); border-radius: 10px; }
  .project-list span { overflow-wrap: anywhere; }
  .project-list button { flex-shrink: 0; }
  .message { margin: 12px 0 0; padding: 9px 12px; background: var(--forest-floor); border-radius: 8px; font-size: 14px; }
  .error { color: var(--forest-error, #8a211a); }
  @media (max-width: 500px) {
    .folder-choice { grid-template-columns: minmax(0, 1fr); }
    .folder-choice button { grid-row: 1; }
    .actions button { width: 100%; }
    .project-list li { align-items: stretch; flex-direction: column; gap: 4px; }
  }
</style>
