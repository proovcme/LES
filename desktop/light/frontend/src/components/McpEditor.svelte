<script lang="ts">
  import { onMount } from 'svelte';
  import Modal from './Modal.svelte';
  import { errorMessage, request } from '../lib/api';

  type Tool = { name: string; title?: string; annotations?: { readOnlyHint?: boolean } };
  type Connection = { id: string; name: string; transport: 'http' | 'stdio'; url: string;
    command?: string; args?: string[]; tools: Tool[] };
  let { close }: { close: () => void } = $props();
  let list = $state<Connection[]>([]);
  let id = $state('');
  let name = $state('');
  let transport = $state<Connection['transport']>('http');
  let url = $state('');
  let command = $state('');
  let args = $state<string[]>([]);
  let found = $state<Tool[]>();
  let selected = $state<string[]>([]);
  let removing = $state(false);
  let busy = $state('');
  let error = $state('');
  let notice = $state('');
  const base = '/api/mcp';
  const endpoint = $derived(base + '/' + encodeURIComponent(id));
  const saved = $derived(list.find((item) => item.id === id));
  const dirty = $derived(saved && (saved.name !== name.trim() || saved.transport !== transport
    || (transport === 'http' ? saved.url !== url.trim()
      : saved.command !== command.trim() || JSON.stringify(saved.args ?? []) !== JSON.stringify(args))));
  const send = <T,>(path: string, method: string, value?: object) =>
    request<T>(path, { method, body: value ? JSON.stringify(value) : undefined, signal: AbortSignal.timeout(35000) });

  async function run(message: string, fn: () => Promise<void>) {
    if (busy) return;
    busy = message; error = ''; notice = '';
    try { await fn(); } catch (e) { error = errorMessage(e); } finally { busy = ''; }
  }
  async function load() { list = (await request<{ connections: Connection[] }>(base)).connections; }
  function edit(item?: Connection) {
    id = item?.id ?? ''; name = item?.name ?? ''; transport = item?.transport ?? 'http';
    url = item?.transport === 'http' ? item.url : ''; command = item?.command ?? ''; args = [...(item?.args ?? [])];
    found = undefined; selected = item?.tools.map((tool) => tool.name) ?? []; removing = false;
  }
  onMount(() => { void run('Загружаю подключения…', load); });
  async function save() {
    const item = await send<Connection>(id ? endpoint : base, id ? 'PUT' : 'POST',
      { name: name.trim(), transport, ...(transport === 'http' ? { url: url.trim() } : { command: command.trim(), args }) });
    edit(item); await load(); notice = 'Подключение сохранено.';
  }
  async function probe() {
    found = (await send<{ tools: Tool[] }>(endpoint + '/probe', 'POST')).tools;
    selected = (saved?.tools ?? []).filter((old) => found?.some((tool) => tool.name === old.name && tool.annotations?.readOnlyHint)).map((tool) => tool.name);
    notice = found.length ? 'Выберите инструменты, которые можно использовать в профилях.' : 'Сервер подключён, список инструментов пуст.';
  }
  async function enable() {
    await send(endpoint + '/tools', 'PUT', { names: selected });
    await load(); notice = 'Доступ к инструментам сохранён.';
  }
</script>

<Modal title="Подключения MCP" {close} editor beforeClose={async () => { if (busy) throw new Error(); }}>
  <p role="status">{busy || notice}</p>
  {#if error}<p role="alert">{error} <button class="quiet" disabled={!!busy} onclick={() => run('Обновляю список…', load)}>Обновить список</button></p>{/if}
  <form class="editor-form" onsubmit={(event) => { event.preventDefault(); void run('Сохраняю…', save); }}>
    <label>Подключение <select value={id} disabled={!!busy} onchange={(event) => edit(list.find((item) => item.id === event.currentTarget.value))}>
      <option value="">Добавить подключение</option>
      {#each list as item (item.id)}<option value={item.id}>{item.name}</option>{/each}
    </select></label>
    <label>Название <input required maxlength="120" bind:value={name} disabled={!!busy} /></label>
    <label>Тип <select bind:value={transport} disabled={!!busy}><option value="http">Адрес сервера</option><option value="stdio">Локальная программа</option></select></label>
    {#if transport === 'http'}
      <label>Адрес MCP <input type="url" required maxlength="2048" bind:value={url} disabled={!!busy} placeholder="http://localhost:8000/mcp" /></label>
    {:else}
      <label>Полный путь к программе MCP <input required maxlength="4096" bind:value={command} disabled={!!busy} /></label>
      {#each args as _, index (index)}
        <div class="argument"><label class="field">Аргумент {index + 1}<input maxlength="4096" bind:value={args[index]} disabled={!!busy} /></label>
          <button type="button" class="quiet" disabled={!!busy} aria-label={`Удалить аргумент ${index + 1}`} onclick={() => args = args.filter((_, i) => i !== index)}>Удалить</button></div>
      {/each}
      <button type="button" class="quiet" disabled={!!busy || args.length >= 32} onclick={() => args = [...args, '']}>Добавить аргумент</button>
    {/if}
    {#if dirty && saved?.tools.length}<small>При изменении адреса или команды инструменты нужно выбрать заново.</small>{/if}
    <div class="modal-footer">
      <button disabled={!!busy || (!!id && !dirty)}>Сохранить подключение</button>
      {#if id}
        <button type="button" class="quiet" disabled={!!busy || !!dirty} onclick={() => run('Проверяю подключение…', probe)}>Проверить и получить инструменты</button>
        <button type="button" class="quiet" disabled={!!busy} onclick={() => removing = true}>Удалить подключение</button>
      {/if}
    </div>
  </form>
  {#if removing}<p>Удалить «{saved?.name}» и отключить его инструменты?</p>
    <div class="modal-footer"><button class="quiet" disabled={!!busy} onclick={() => removing = false}>Отмена</button>
      <button disabled={!!busy} onclick={() => run('Удаляю…', async () => {
        await send(endpoint, 'DELETE'); edit(); await load(); notice = 'Подключение удалено.';
      })}>Подтвердить удаление</button></div>
  {/if}
  {#if found}
    {#each found as tool (tool.name)}
      <label class="option"><input type="checkbox" value={tool.name} bind:group={selected} disabled={!!busy || !tool.annotations?.readOnlyHint} />
        <span>{tool.title || tool.name}{#if !tool.annotations?.readOnlyHint}<small>Сервер не пометил инструмент как чтение</small>{/if}</span>
      </label>
    {/each}
    <button disabled={!!busy || !!dirty || selected.length > 32} onclick={() => run('Сохраняю доступ…', enable)}>Сохранить выбор инструментов</button>
  {:else if saved?.tools.length}
    <p>Разрешены: {saved.tools.map((tool) => tool.title || tool.name).join(', ')}.</p>
    <button class="quiet" disabled={!!busy || !!dirty} onclick={() => run('Отключаю инструменты…', async () => {
      await send(endpoint + '/tools', 'PUT', { names: [] }); await load(); notice = 'Инструменты отключены.';
    })}>Отключить все инструменты</button>
  {/if}
</Modal>

<style>
  form { display: grid; gap: 12px; margin-block: 12px; }
  form > button { justify-self: start; }
  .argument { display: flex; align-items: end; gap: 8px; }
  .argument label { flex: 1; min-width: 0; }
</style>
