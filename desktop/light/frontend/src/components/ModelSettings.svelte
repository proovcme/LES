<script lang="ts">
  import { onMount } from 'svelte';
  import { ApiError, errorMessage, request } from '../lib/api';
  import Modal from './Modal.svelte';

  let { close }: { close: () => void } = $props();
  type Role = 'answer' | 'embeddings' | 'local_fallback';
  type Locality = 'loopback' | 'private_network' | 'remote';
  type Connection = { connection_id: string; revision_id: string; display_name: string; base_url: string;
    model_id: string; locality: Locality; enabled: boolean; secret_status: string;
    extension_type: string | null; requested_context_tokens: number | null };
  type Engine = { id: string; name: string; base_url: string; extension_type: string | null; models: string[] };
  type Registry = { connections: Connection[]; bindings: Record<Role, { binding_revision: number; connection_revision_id: string } | null> };
  const route = '/api/model-connections';
  const roles: { id: Role; label: string }[] = [
    { id: 'answer', label: 'Ответы в чате' }, { id: 'embeddings', label: 'Поиск по документам' },
    { id: 'local_fallback', label: 'Резервная локальная модель' },
  ];
  let data = $state<Registry | null>(null);
  let effective = $state<Partial<Record<Role,{ model_id: string | null; status: string } | null>>>({});
  let busy = $state('');
  let status = $state('');
  let error = $state('');
  let engines = $state<Engine[]>([]);
  let engineID = $state('');
  let choices = $state<string[]>([]);
  let form = $state<'local' | 'api' | 'edit' | 'key' | null>(null);
  let target = $state<Connection | null>(null);
  let confirmation = $state<string | null>(null);
  let name = $state(''); let address = $state(''); let model = $state('');
  let key = $state(''); let locality = $state<Locality>('remote');
  let context = $state<number | undefined>(undefined);
  let extension = $state<string | null>(null);
  let tested = $state<{ name: string; state: string }[]>([]);
  let testModel = $state('');
  const labels: Record<string,string> = { models:'Список моделей',chat_completions:'Ответы',streaming:'Поток',
    tools:'Инструменты',structured_output:'Структурированный ответ',embeddings:'Поиск' };

  const post = <T,>(path: string, payload: object, method = 'POST', timeout = 15000) =>
    request<T>(route + path, { method, body: JSON.stringify(payload), signal: AbortSignal.timeout(timeout) });
  const refresh = async () => {
    data = await request<Registry>(route);
    effective = (await request<{roles: typeof effective}>(route + '/effective')).roles;
  };
  onMount(() => { void perform('Загружаю подключения…', refresh); return () => { key = ''; }; });
  async function perform(label: string, work: () => Promise<void>) {
    if (busy) return;
    busy = label; error = ''; status = '';
    try { await work(); } catch (failure) { error = errorMessage(failure); }
    finally { busy = ''; }
  }
  function clear() {
    target = null; name = ''; address = ''; model = ''; key = ''; context = undefined;
    locality = 'remote'; extension = null; engineID = ''; choices = []; confirmation = null;
  }
  function openAPI() { clear(); form = 'api'; }
  function discover() {
    clear(); form = 'local';
    void perform('Ищу запущенные движки…', async () => {
      const result = await post<{ engines: Engine[] }>('/discover-local', {});
      engines = result.engines.filter((engine) => engine.models.length);
      if (engines.length === 1) chooseEngine(engines[0].id);
      if (!engines.length) status = 'Запустите локальный движок или укажите адрес API.';
    });
  }
  function chooseEngine(id: string) {
    engineID = id;
    const engine = engines.find((item) => item.id === id);
    address = engine?.base_url ?? ''; extension = engine?.extension_type ?? null;
    choices = engine?.models ?? []; locality = 'loopback'; model = choices.length === 1 ? choices[0] : '';
  }
  function edit(connection: Connection, onlyKey = false) {
    clear(); form = onlyKey ? 'key' : 'edit'; target = connection;
    name = connection.display_name; address = connection.base_url; model = connection.model_id;
    locality = connection.locality; context = connection.requested_context_tokens ?? undefined;
    extension = connection.extension_type;
  }
  function findModels() {
    void perform('Читаю список моделей…', async () => {
      const result = await post<{ status: string; models?: string[]; message?: string }>('/discover-models',
        { base_url: address.trim(), locality, ...(key ? { secret_value: key } : {}) });
      if (result.status !== 'ok') throw new ApiError(result.message || 'Список моделей недоступен.');
      choices = result.models || [];
      status = choices.length ? 'Выберите модель из списка.' : 'Список пуст. Имя можно указать вручную.';
    });
  }
  function save(event: SubmitEvent) {
    event.preventDefault();
    void perform('Сохраняю подключение…', async () => {
      const id = target ? '/' + encodeURIComponent(target.connection_id) : '';
      const fields = { display_name: name.trim() || model.trim(), base_url: address.trim(), model_id: model.trim(),
        locality, extension_type: extension, requested_context_tokens: context ?? null };
      if (form === 'key') await post(id + '/secret', { expected_revision_id: target?.revision_id, secret_value: key });
      else if (form === 'edit') await post(id + '/revisions', { ...fields, expected_revision_id: target?.revision_id });
      else await post('', { ...fields, ...(key ? { secret_value: key } : {}) });
      key = ''; form = null; await refresh(); status = 'Подключение сохранено.';
    });
  }
  function probe(connection: Connection, role: Role) {
    const capabilities = role === 'embeddings' ? ['models', 'embeddings'] :
      ['models', 'chat_completions', 'streaming', 'tools', 'structured_output'];
    return post<{ capabilities: { name: string; state: string }[] }>('/' + encodeURIComponent(connection.connection_id) + '/test',
      { revision_id: connection.revision_id, capabilities }, 'POST', 160000);
  }
  function assign(connection: Connection, role: Role) {
    void perform('Проверяю модель и назначаю роль…', async () => {
      const payload = { connection_revision_id: connection.revision_id,
        expected_binding_revision: data?.bindings[role]?.binding_revision ?? null };
      try { await post('/roles/' + role, payload, 'PUT'); }
      catch (failure) {
        if (!(failure instanceof ApiError) || !failure.code.startsWith('CAPABILITY_')) throw failure;
        const result = await probe(connection, role);
        testModel = connection.model_id; tested = result.capabilities.filter((cap) => cap.name in labels);
        const required = role === 'embeddings' ? 'embeddings' : 'chat_completions';
        if (!tested.some((cap) => cap.name === required && cap.state === 'supported')) {
          throw new ApiError('Модель не подтвердила нужную возможность. Проверьте имя модели, ключ и доступность сервиса.');
        }
        await post('/roles/' + role, payload, 'PUT');
      }
      await refresh(); status = 'Модель назначена.';
    });
  }
  function disable(connection: Connection) {
    void perform('Отключаю модель…', async () => {
      await post('/' + encodeURIComponent(connection.connection_id) + '/disable',
        { expected_revision_id: connection.revision_id, confirm_bound_roles: true });
      confirmation = null; await refresh(); status = 'Подключение отключено.';
    });
  }
  function label(role: Role) {
    if (!data?.bindings[role]) return 'Не назначено';
    const row = effective[role];
    return row?.model_id ? row.model_id + (row.status === 'blocked' ? ' · требует проверки' : '') : 'Подключение недоступно';
  }
</script>

<Modal title="Модели" {close} beforeClose={async () => { if (busy) throw new Error('busy'); }}>
  <p>Подключите модель и явно выберите, для чего Лес её использует.</p>
  <dl class="model-roles">{#each roles.slice(0,2) as role}<div><dt>{role.label}</dt><dd>{label(role.id)}</dd></div>{/each}</dl>
  <div class="source-actions"><button disabled={!!busy} onclick={discover}>Найти на компьютере</button><button class="quiet" disabled={!!busy} onclick={openAPI}>Подключить API</button></div>
  {#if busy || status}<p class="activity" role="status">{busy || status}</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if form}
    <form class="model-form" onsubmit={save}>
      <h3>{form === 'key' ? 'Обновить ключ' : form === 'edit' ? 'Изменить подключение' : 'Подключение модели'}</h3>
      {#if form === 'local'}
        <label>Движок <select bind:value={engineID} disabled={!!busy} onchange={(event) => chooseEngine(event.currentTarget.value)}><option value="">Выберите движок</option>
          {#each engines as engine (engine.id)}<option value={engine.id}>{engine.name}</option>{/each}
        </select></label>
      {:else if form !== 'key'}
        <label>Адрес API <input type="url" bind:value={address} required placeholder="https://адрес/v1" /></label>
        <label>Расположение <select bind:value={locality}><option value="remote">Внешний сервис</option><option value="loopback">Этот компьютер</option><option value="private_network">Локальная сеть</option></select></label>
      {/if}
      {#if form === 'api' || form === 'key'}<label>API-ключ <input type="password" bind:value={key} required={form === 'key'} autocomplete="off" /></label>{/if}
      {#if form === 'api'}<button type="button" class="quiet" disabled={!!busy || !address} onclick={findModels}>Получить модели</button>{/if}
      {#if form !== 'key'}
        <label>Имя модели <input bind:value={model} list="known-models" required maxlength="300" /></label>
        <datalist id="known-models">{#each choices as item}<option value={item}></option>{/each}</datalist>
        <details><summary>Дополнительно</summary>
          <label>Название подключения <input bind:value={name} maxlength="160" /></label>
          <label>Контекст, токенов <input type="number" bind:value={context} min="1" placeholder="Автоматически" /></label>
        </details>
      {/if}
      <div class="source-actions"><button type="button" class="quiet" disabled={!!busy} onclick={() => { form = null; key = ''; }}>Отмена</button>
        <button disabled={!!busy || (form !== 'key' && (!address || !model))}>Сохранить</button></div>
    </form>
  {/if}
  {#if data}
    <ul class="model-connections">
      {#each data.connections as row (row.connection_id)}
        <li><details>
          <summary><strong>{row.model_id}</strong> · {row.extension_type === 'ollama' ? 'Ollama' : row.extension_type === 'lm_studio' ? 'LM Studio' : 'Совместимый API'}{row.enabled ? '' : ' · отключена'}</summary>
          <p class="muted">{row.display_name} · {row.base_url}</p>
          {#if row.enabled}
            <div class="source-actions">{#each roles as role}
              {#if role.id !== 'local_fallback' || row.locality !== 'remote'}
                <button class="quiet" disabled={!!busy || data.bindings[role.id]?.connection_revision_id === row.revision_id} onclick={() => assign(row, role.id)}>{role.id === 'answer' ? 'Назначить для чата' : role.id === 'embeddings' ? 'Назначить для поиска' : 'Назначить резервной'}</button>
              {/if}
            {/each}</div>
            <div class="source-actions"><button class="quiet" disabled={!!busy} onclick={() => edit(row)}>Изменить</button>
              <button class="quiet" disabled={!!busy} onclick={() => edit(row,true)}>Обновить ключ</button>
              <button class="quiet" disabled={!!busy} onclick={() => confirmation = row.connection_id}>Отключить</button></div>
            {#if confirmation === row.connection_id}
              <p>Если модель назначена, её роль перестанет работать, пока вы не выберете другую.</p>
              <div class="source-actions"><button class="quiet" disabled={!!busy} onclick={() => confirmation = null}>Отмена</button><button disabled={!!busy} onclick={() => disable(row)}>Подтвердить отключение</button></div>
            {/if}
          {/if}
        </details></li>
      {/each}
    </ul>
  {/if}
  {#if tested.length}<details><summary>Проверка: {testModel}</summary><ul>{#each tested as item}<li>{labels[item.name]}: {item.state === 'supported' ? 'подтверждено' : item.state === 'unsupported' ? 'не поддерживается' : 'не подтверждено'}</li>{/each}</ul></details>{/if}
</Modal>

<style>
  .model-roles { display: grid; grid-template-columns: repeat(auto-fit,minmax(155px,1fr)); gap: 10px; }
  .model-roles div { background: var(--forest-floor); padding: 10px 12px; border-radius: 10px; }
  dt { color: var(--forest-muted); font-size: 12px; } dd { margin: 0; font-weight: 650; overflow-wrap: anywhere; }
  .model-form, .model-connections { margin: 16px 0; }
  .model-form { display: grid; gap: 10px; border-block: 1px solid var(--forest-line); padding: 12px 0; }
  .model-form h3 { margin: 0; font-size: 15px; }
  .model-form label { display: grid; gap: 4px; font-size: 13px; }
  .model-form :is(input,select) {
    width: 100%; min-height: 44px; border: 1px solid var(--forest-line);
    border-radius: 10px; padding: 9px 12px; background: white;
  }
  .model-connections { list-style: none; padding: 0; }
  .model-connections li { padding: 12px 0; border-bottom: 1px solid var(--forest-line); }
  .model-connections .source-actions { margin-top: 6px; }
  summary { cursor: pointer; }
</style>
