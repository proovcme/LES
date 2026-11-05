<script lang="ts">
  import { untrack, onDestroy } from 'svelte';
  import { request, errorMessage, ApiError } from '../lib/api';
  import Modal from './Modal.svelte';
  import Icon from './Icon.svelte';
  type Area = 'user' | 'memory' | 'files';
  type Document = { area: Area; project_id: number; path: string; revision: string; text: string; exists: boolean };
  type History = { id: string; at: string; actor: string; reason: string; status: string; exists: boolean; before_exists: boolean };
  let { open = $bindable(false), projectId = 0, onDirty = (_dirty: boolean) => {} }: { open?: boolean; projectId?: number; onDirty?: (dirty: boolean) => void } = $props();
  let area = $state<Area>('user'); let project = $state(0);
  let projects = $state<{ id: number; name: string }[]>([]); let files = $state<{ path: string; bytes: number; exists?: boolean }[]>([]);
  let doc = $state<Document | null>(null); let text = $state(''); let savedText = $state(''); let filePath = $state(''); let newFilePath = $state('');
  let loading = $state(false); let saving = $state(false); let error = $state(''); let status = $state('');
  let confirmDelete = $state(false);
  let addingFile = $state(false);
  let actionBusy = $state(false); let actionLabel = $state(''); let actionPending: Promise<void> | undefined;
  let policy = $state({ auto_memory: true, workspace_write: true }); let policyRevision = $state('missing');
  let remote = $state<Document | null>(null); let showHistory = $state(false); let history = $state<History[]>([]);
  let historyOffset = $state(0); let moreHistory = $state(false);
  let preview = $state<{ id: string; text: string; before: string; exists: boolean; before_exists: boolean } | null>(null); let previewSide = $state<'before' | 'after'>('after');
  let timer: ReturnType<typeof setTimeout>; let pending: Promise<void> | undefined;
  const dirty = $derived(!!doc && text !== savedText);
  const busy = $derived(loading || saving || actionBusy);
  const title = $derived(area === 'user' ? 'О вас' : area === 'memory' ? 'Что Лес помнит' : 'Рабочие файлы');
  $effect(() => { onDirty(dirty || saving || actionBusy); });
  $effect(() => { if (open) untrack(() => { void initialize(); }); });
  onDestroy(() => clearTimeout(timer));

  function url(action: string, current = doc, extra: Record<string, string> = {}) {
    return '/api/workspace/agent-state?' + new URLSearchParams({ action, area: current?.area || area,
      project_id: String(current?.project_id ?? project), path: current?.area === 'files' ? filePath : '', ...extra });
  }
  async function post(body: object) { return request<{ revision: string }>('/api/workspace/agent-state', { method: 'POST', body: JSON.stringify(body) }); }
  async function initialize() {
    loading = true; error = '';
    try {
      const [all, settings] = await Promise.all([
        request<{ projects: { id: number; name: string }[] }>('/api/projects'),
        request<{ value: typeof policy; revision: string }>('/api/workspace/agent-state?action=policy'),
      ]);
      projects = all.projects; policy = settings.value; policyRevision = settings.revision;
      project = projects.some(p => p.id === projectId) ? projectId : 0;
      if (!dirty) await load();
    } catch (e) { error = errorMessage(e); }
    finally { loading = false; }
  }
  async function load() {
    loading = true; error = ''; remote = null; preview = null; showHistory = false; confirmDelete = false;
    try {
      if (area === 'files') {
        files = (await request<{ files: typeof files }>(url('list', null, { include_deleted: 'true' }))).files;
        if (!filePath && files.length) filePath = files[0].path;
        if (!filePath) { doc = null; text = savedText = ''; return; }
      }
      const current = await request<Document>(url('read', { area, project_id: area === 'user' ? 0 : project } as Document));
      doc = current; text = savedText = current.text; status = current.exists ? 'Сохранено' : area === 'files' ? 'Файл в истории' : 'Пока нет записей';
    } finally { loading = false; }
  }
  async function changeArea(next: Area) { const previous = area; try { await flush(); area = next; await load(); } catch (e) { area = previous; error = errorMessage(e); } }
  async function changeProject(next: number) { const previous = project; const previousPath = filePath; try { await flush(); project = next; filePath = ''; await load(); } catch (e) { project = previous; filePath = previousPath; error = errorMessage(e); } }
  function changed() { clearTimeout(timer); status = 'Сохраняю…'; timer = setTimeout(() => { void flush().catch(() => undefined); }, 700); }
  export async function flush(): Promise<void> {
    if (actionPending) await actionPending;
    await flushText();
  }
  async function runAction(label: string, callback: () => Promise<void>) {
    if (actionBusy) return;
    actionBusy = true; actionLabel = label; error = '';
    const current = (async () => { await flushText(); await callback(); })();
    actionPending = current;
    try { await current; } catch (e) { error = errorMessage(e); }
    finally { actionPending = undefined; actionBusy = false; actionLabel = ''; }
  }
  async function flushText(): Promise<void> {
    clearTimeout(timer); if (pending) await pending;
    if (!doc || text === savedText) return;
    if (remote) throw new ApiError('Сначала сравните версии памяти.', 409);
    const current = doc; const value = text; saving = true; error = '';
    pending = (async () => {
      try {
        const result = await post({ action: 'save', area: current.area, project_id: current.project_id,
          path: current.area === 'files' ? filePath : '', expected_revision: current.revision, text: value, reason: 'Правка в редакторе Леса' });
        doc = { ...current, revision: result.revision, exists: true }; savedText = value; status = 'Сохранено';
      } catch (e) {
        error = errorMessage(e); status = 'Пока не сохранено';
        if (e instanceof ApiError && e.status === 409) remote = await request<Document>(url('read', current)).catch(() => null);
        throw e;
      } finally { saving = false; }
    })();
    try { await pending; } finally { pending = undefined; }
    if (text !== savedText) await flushText();
  }
  async function setPolicy(key: keyof typeof policy, value: boolean) {
    await runAction('Сохраняю настройки…', async () => { const next = { ...policy, [key]: value }; const result = await post({ action: 'policy_save', project_id: 0,
      area: 'policy', expected_revision: policyRevision, value: next, reason: 'Настройки агента' });
      policy = next; policyRevision = result.revision; });
  }
  async function chooseFile(path: string) { const previous = filePath; try { await flush(); filePath = path; await load(); } catch (e) { filePath = previous; error = errorMessage(e); } }
  async function createFile() {
    await runAction('Создаю рабочий файл…', async () => { const created = await post({ action: 'save', area: 'files', project_id: project, path: newFilePath,
      expected_revision: 'missing', text: '', reason: 'Новый рабочий файл' });
      filePath = newFilePath; newFilePath = ''; addingFile = false;
      doc = { area: 'files', project_id: project, path: filePath, revision: created.revision, text: '', exists: true }; text = savedText = ''; await load(); });
  }
  async function openHistory(offset = 0) { try { await flush(); historyOffset = offset;
    const result = await request<{ items: History[]; more: boolean }>(url('history', doc, { offset: String(offset) }));
    history = result.items; moreHistory = result.more; showHistory = true; preview = null;
  } catch (e) { error = errorMessage(e); } }
  async function viewVersion(id: string) { try { preview = await request(url('version', doc, { history_id: id })); previewSide = 'after'; } catch (e) { error = errorMessage(e); } }
  async function restore() { if (!preview || !doc) return;
    await runAction('Восстанавливаю версию…', async () => { await post({ action: 'restore', area: doc!.area, project_id: doc!.project_id, path: area === 'files' ? filePath : '',
      expected_revision: doc!.revision, history_id: preview!.id, side: previewSide, reason: 'Откат через историю изменений' }); await load(); });
  }
  async function removeFile() {
    await runAction('Убираю файл в историю…', async () => {
      if (!doc || area !== 'files' || !doc.exists) return;
      await post({ action: 'delete', area: 'files', project_id: doc.project_id, path: filePath,
        expected_revision: doc.revision, reason: 'Удаление рабочего файла через редактор' });
      await load();
    });
  }
  async function downloadFile() {
    await runAction('Готовлю файл для скачивания…', async () => {
      if (!doc || area !== 'files' || !doc.exists) return;
      const value = await request<{ filename: string; text: string }>(url('export', doc, { expected_revision: doc.revision }));
      const objectURL = URL.createObjectURL(new Blob([value.text], { type: 'text/plain;charset=utf-8' }));
      const link = document.createElement('a'); link.href = objectURL; link.download = value.filename;
      document.body.append(link); link.click(); link.remove();
      setTimeout(() => URL.revokeObjectURL(objectURL), 1000);
    });
  }
</script>

{#if open}
  <Modal title="Лес помнит" wide beforeClose={flush} close={() => { open = false; }}>
    <p class="memory-intro">{area === 'user' ? 'Как отвечать и что учитывать в работе с вами.' : area === 'memory' ? 'Важное между разговорами. Изменения сохраняются в истории.' : 'Файлы, с которыми работает Лес. Исходные документы остаются в своих папках.'}</p>
    <div class="memory-tabs" aria-label="Раздел памяти">
      {#each [['user', 'О вас'], ['memory', 'Память'], ['files', 'Рабочие файлы']] as [key, label]}
        <button class:active={area === key} class="quiet" aria-pressed={area === key} disabled={busy} onclick={() => changeArea(key as Area)}>{label}</button>
      {/each}
    </div>
    {#if area !== 'user'}<label for="memory-project">Область</label><select id="memory-project" value={project} disabled={busy} onchange={(e) => changeProject(Number(e.currentTarget.value))}>
      <option value={0}>Общая</option>{#each projects as p}<option value={p.id}>{p.name}</option>{/each}
    </select>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if actionBusy}<p class="muted" role="status">{actionLabel}</p>{/if}
    {#if loading}<p role="status">Открываю память…</p>
    {:else if showHistory}
      <button class="quiet" onclick={() => { showHistory = false; preview = null; }}><Icon name="back" />К редактору</button>
      {#if preview}
        <div class="memory-tabs"><button class="quiet" aria-pressed={previewSide === 'before'} onclick={() => previewSide = 'before'}>До изменения</button><button class="quiet" aria-pressed={previewSide === 'after'} onclick={() => previewSide = 'after'}>После изменения</button></div>
        <label for="memory-preview">Сохранённая версия</label><textarea id="memory-preview" readonly rows="9" value={previewSide === 'before' ? preview.before : preview.text}></textarea>
        <p class="muted">{(previewSide === 'before' ? preview.before_exists : preview.exists) ? 'Текущее содержимое останется в истории.' : 'В этой версии файл отсутствует. Откат уберёт его из рабочего каталога, сохранив историю.'}</p>
        <button disabled={busy} onclick={restore}>Вернуть эту версию</button>
      {:else}
        <ul class="memory-history">{#each history as entry}<li><button class="quiet" onclick={() => viewVersion(entry.id)}><span>{new Date(entry.at).toLocaleString('ru-RU')} · {entry.actor === 'agent' ? 'Лес' : 'Вы'}</span><small>{entry.status === 'conflict' ? 'Конфликт с внешним редактором' : entry.reason || 'Изменение памяти'}</small></button></li>{:else}<li class="muted">Изменений пока нет.</li>{/each}</ul>
        <div class="modal-footer">{#if historyOffset > 0}<button class="quiet" onclick={() => openHistory(Math.max(0, historyOffset - 50))}>Новее</button>{/if}{#if moreHistory}<button class="quiet" onclick={() => openHistory(historyOffset + 50)}>Раньше</button>{/if}</div>
      {/if}
    {:else}
      {#if area === 'files'}
        {#if files.length}<label for="workfile-list">Открыть файл</label><select id="workfile-list" value={filePath} disabled={busy} onchange={(e) => chooseFile(e.currentTarget.value)}>{#each files as file}<option value={file.path}>{file.path}{file.exists === false ? ' — в истории' : ''}</option>{/each}</select>{/if}
        {#snippet newFileForm()}<div class="memory-file-name"><label for="workfile-name">Имя нового файла<input id="workfile-name" bind:value={newFilePath} disabled={busy} placeholder="notes/план.md" /></label><button class="quiet" disabled={!newFilePath.trim() || busy} onclick={createFile}>Создать</button></div>{/snippet}
        {#if files.length}<details class="new-file" bind:open={addingFile}><summary>Новый файл</summary>{@render newFileForm()}</details>{:else}{@render newFileForm()}{/if}
      {/if}
      {#if doc}<label for="agent-memory-text">{title}{area === 'files' ? ` · ${filePath}` : ''}</label>
        <textarea class="memory-editor" id="agent-memory-text" bind:value={text} oninput={changed} rows="10" disabled={actionBusy} readonly={area === 'files' && !doc.exists} maxlength={area === 'files' ? 1048576 : 32768} placeholder={area === 'user' ? 'Как к вам обращаться, как отвечать, что учитывать в работе…' : area === 'memory' ? 'Важное, к чему вы хотите возвращаться между разговорами…' : doc.exists ? 'Содержимое рабочего файла…' : 'Файл можно восстановить через историю изменений.'}></textarea>
        <div class="memory-save"><span role="status">{status}</span><button class="quiet" disabled={busy} onclick={() => openHistory()}>История изменений</button></div>
        {#if area === 'files' && doc.exists}
          <div class="file-actions"><button class="quiet" disabled={busy} onclick={downloadFile}>Скачать файл</button><button class="quiet" disabled={busy} onclick={() => confirmDelete = true}>Убрать файл</button></div>
          {#if confirmDelete}<div class="file-removal" role="group" aria-label="Удаление рабочего файла"><p>Убрать «{filePath}» из рабочих файлов? Его можно вернуть через историю изменений.</p><div class="modal-footer"><button class="quiet" disabled={busy} onclick={() => confirmDelete = false}>Оставить</button><button disabled={busy} onclick={removeFile}>Убрать в историю</button></div></div>{/if}
        {/if}
      {:else}<p class="muted">Создайте рабочий файл. Исходные документы остаются в своих папках.</p>{/if}
      {#if remote}<label for="remote-memory">Версия из другого окна или редактора</label><textarea id="remote-memory" value={remote.text} readonly rows="5"></textarea>
        <div class="modal-footer"><button class="quiet" onclick={() => { doc = remote; text = savedText = remote!.text; remote = null; error = ''; }}>Загрузить сохранённую</button><button onclick={() => { doc = remote; remote = null; void flush().catch(() => undefined); }}>Оставить мой текст</button></div>
      {:else if error && dirty}<button class="quiet" onclick={() => flush().catch(() => undefined)}>Повторить сохранение</button>{/if}
      <details class="memory-settings"><summary>Что Лес может делать сам</summary>
        <label class="option"><input type="checkbox" disabled={busy} checked={policy.auto_memory} onchange={(e) => setPolicy('auto_memory', e.currentTarget.checked)} /><span><strong>Поддерживать память</strong><small>Обновлять предпочтения и рабочий контекст с историей изменений.</small></span></label>
        <label class="option"><input type="checkbox" disabled={busy} checked={policy.workspace_write} onchange={(e) => setPolicy('workspace_write', e.currentTarget.checked)} /><span><strong>Создавать рабочие файлы</strong><small>Редактировать файлы в собственном каталоге Леса.</small></span></label>
        <p class="muted">О вас — USER.md. Память — MEMORY.md, отдельно для каждой области. Память помогает продолжать работу, источники подтверждают ответ.</p>
      </details>
    {/if}
  </Modal>
{/if}

<style>
  .memory-intro { color: var(--forest-muted); margin: 0 0 20px; line-height: 1.6; }
  .memory-tabs { display: flex; flex-wrap: wrap; gap: 8px; margin: 0 0 20px; }
  .memory-tabs button { flex: 1; justify-content: center; min-width: 120px; }
  .memory-tabs button.active { background: var(--forest-selected); color: var(--forest-ink); }
  .memory-save { display: flex; align-items: center; justify-content: space-between; gap: 12px; color: var(--forest-muted); font-size: .875rem; }
  .memory-settings { margin-top: 24px; padding-top: 16px; border-top: 1px solid var(--forest-line); }
  summary { cursor: pointer; padding: 12px 0; }
  .memory-history { list-style: none; padding: 0; margin: 16px 0; }
  .memory-history button { display: flex; flex-direction: column; align-items: start; width: 100%; padding: 12px; }
  .memory-history small { color: var(--forest-muted); white-space: normal; text-align: left; }
  .memory-editor { min-height: 260px; max-height: 50dvh; padding: 16px; background: var(--forest-floor); border: 1px solid var(--forest-line); resize: vertical; margin: 8px 0; }
  select, input { width: 100%; min-width: 0; max-width: 100%; min-height: 44px; border: 1px solid var(--forest-line); border-radius: 10px; padding: 8px 12px; background: var(--forest-floor); margin: 8px 0 16px; }
  input[type="checkbox"] { width: 19px; min-height: 19px; margin: 0; }
  .memory-file-name { display: flex; align-items: end; gap: 12px; margin: 12px 0; }
  .memory-file-name label { flex: 1; min-width: 0; margin: 0; }
  .memory-file-name button { margin-bottom: 12px; }
  .new-file { margin-bottom: 12px; }
  .file-actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 12px; }
  .file-removal { padding: 16px; margin-top: 12px; border: 1px solid var(--forest-line); border-radius: 12px; background: var(--forest-floor); }
  .file-removal p { margin: 0; }
  @media(max-width:560px) { .memory-file-name { display: block; } .memory-save { align-items: start; flex-direction: column; gap: 0; } }
</style>
