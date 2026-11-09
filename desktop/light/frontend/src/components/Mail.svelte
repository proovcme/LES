<script lang="ts">
  import { onMount } from 'svelte';
  import { request, errorMessage } from '../lib/api';
  import Modal from './Modal.svelte';

  type Account = { id: string; label: string; kind: string; sync_state: string };
  type Message = { id: string; subject: string; sender: string; received_at: string };
  type Detail = { profile: { text: string }; rag_doc_id: string };
  type Imported = { account_id?: string; messages: number; skipped?: number; skipped_attachments?: number; next_offset?: number | null };
  let { close }: { close: () => void } = $props();
  let accounts = $state<Account[]>([]);
  let messages = $state<Message[]>([]);
  let detail = $state<Detail | null>(null);
  let account = $state('');
  let form = $state<'account' | 'archive' | null>(null);
  let label = $state('');
  let host = $state('');
  let port = $state<number | undefined>(993);
  let login = $state('');
  let password = $state('');
  let path = $state('');
  let nextOffset = $state<number | null>(null);
  let busy = $state('');
  let status = $state('');
  let error = $state('');
  const route = '/api/mail';
  const selected = $derived(accounts.find((item) => item.id === account));

  const post = <T,>(path: string, payload: object, timeout = 30000) =>
    request<T>(route + path, { method: 'POST', body: JSON.stringify(payload), signal: AbortSignal.timeout(timeout) });
  async function perform(message: string, work: () => Promise<void>) {
    if (busy) return;
    busy = message; status = ''; error = '';
    try { await work(); } catch (failure) { error = errorMessage(failure); }
    finally { busy = ''; }
  }
  async function select(id: string) {
    account = id; detail = null; messages = [];
    if (!id) return;
    const result = await request<{ messages: Message[] }>(route + '/messages?' + new URLSearchParams({ account_id: id }));
    messages = result.messages;
  }
  async function refresh(preferred = account) {
    accounts = (await request<{ accounts: Account[] }>(route + '/accounts')).accounts;
    await select(accounts.some((item) => item.id === preferred) ? preferred : accounts[0]?.id ?? '');
  }
  onMount(() => { void perform('Загружаю почту…', refresh); return () => { password = ''; }; });

  function saveAccount() {
    return perform('Сохраняю ящик…', async () => {
      try {
        const result = await post<{ account: Account }>('/accounts', { kind: 'imap', label, host, port, login, password, ssl: true });
        form = null;
        await refresh(result.account.id);
        status = 'Ящик сохранён. Проверьте подключение.';
      } finally { password = ''; }
    });
  }
  function importArchive() {
    return perform('Читаю архив…', async () => {
      const result = await post<Imported>('/import-archive', { path, parse: true, offset: nextOffset ?? 0 }, 240000);
      nextOffset = result.next_offset ?? null;
      if (nextOffset === null) form = null;
      await refresh(result.account_id);
      status = `Прочитано писем: ${result.messages}.${result.skipped ? ` Пропущено писем: ${result.skipped}.` : ''}${result.skipped_attachments ? ` Пропущено вложений: ${result.skipped_attachments}.` : ''}`;
    });
  }
  function accountAction(action: 'test' | 'sync') {
    const id = account;
    return perform(action === 'test' ? 'Проверяю подключение…' : 'Запускаю синхронизацию…', async () => {
      await post('/accounts/' + encodeURIComponent(id) + '/' + action, {});
      await refresh(id);
      status = action === 'test' ? 'Подключение подтверждено.' : 'Синхронизация запущена. Обновите список, чтобы увидеть новые письма.';
    });
  }
</script>

<Modal title="Почта" {close} wide>
  <div class="source-actions">
    <button class="quiet" disabled={!!busy} onclick={() => form = form === 'account' ? null : 'account'}>Подключить ящик</button>
    <button class="quiet" disabled={!!busy} onclick={() => form = form === 'archive' ? null : 'archive'}>Импортировать архив</button>
    <button class="quiet" disabled={!!busy} onclick={() => perform('Обновляю почту…', refresh)}>Обновить</button>
  </div>
  {#if form === 'account'}
    <form onsubmit={(event) => { event.preventDefault(); void saveAccount(); }}>
      <label>Название ящика <input required bind:value={label} maxlength="160" /></label>
      <label>IMAP-сервер <input required bind:value={host} placeholder="imap.example.com" /></label>
      <label>Порт IMAPS <input type="number" required min="1" max="65535" bind:value={port} /></label>
      <label>Имя пользователя <input required bind:value={login} autocomplete="username" /></label>
      <label>Пароль приложения <input required type="password" bind:value={password} autocomplete="new-password" /></label>
      <div class="source-actions"><button disabled={!!busy}>Сохранить</button></div>
    </form>
  {:else if form === 'archive'}
    <form onsubmit={(event) => { event.preventDefault(); void importArchive(); }}>
      <label>Полный путь к архиву Outlook <input required bind:value={path} oninput={() => { nextOffset = null; }} /></label>
      <div class="source-actions"><button disabled={!!busy}>{nextOffset === null ? 'Начать импорт' : 'Продолжить импорт'}</button></div>
    </form>
  {/if}
  {#if accounts.length}
    <label>Почтовый набор
      <select value={account} disabled={!!busy} onchange={(event) => {
        const id = event.currentTarget.value;
        void perform('Читаю письма…', () => select(id));
      }}>
        {#each accounts as item (item.id)}<option value={item.id}>{item.label}</option>{/each}
      </select>
    </label>
    {#if selected?.kind === 'imap'}
      <div class="source-actions">
        <button class="quiet" disabled={!!busy} onclick={() => accountAction('test')}>Проверить подключение</button>
        <button class="quiet" disabled={!!busy || selected.sync_state === 'running'} onclick={() => accountAction('sync')}>Синхронизировать</button>
      </div>
    {/if}
    {#if detail}
      <button class="quiet" onclick={() => detail = null}>К списку писем</button>
      <pre>{detail.profile.text}</pre>
      {#if detail.rag_doc_id}<a href={'/api/documents/by-id/' + encodeURIComponent(detail.rag_doc_id) + '/raw'}>Открыть сохранённое письмо</a>{/if}
    {:else if messages.length}
      <ul class="mail-messages">
        {#each messages as item (item.id)}
          <li><button class="quiet" disabled={!!busy} onclick={() => perform('Открываю письмо…', async () => {
            detail = await request<Detail>(route + '/messages/' + encodeURIComponent(item.id));
          })}>{item.subject || 'Без темы'}</button><span>{item.sender}</span></li>
        {/each}
      </ul>
    {:else if !busy}
      <p class="muted">В этом наборе пока нет писем.</p>
    {/if}
  {:else if !busy}
    <p class="muted">Подключите IMAP-ящик или импортируйте архив.</p>
  {/if}
  {#if error}<p role="alert">{error}</p>{/if}
  <p class="muted" role="status">{busy || status}</p>
</Modal>

<style>
  form { display: grid; gap: 10px; margin-block: 12px; padding-block: 12px; border-block: 1px solid var(--forest-line); }
  label { display: grid; gap: 4px; }
  :is(input, select) { min-width: 0; padding: 8px; border: 1px solid var(--forest-line); border-radius: 8px; font: inherit; }
  .mail-messages { display: grid; gap: 6px; padding: 0; list-style: none; }
  li { display: grid; gap: 3px; padding-block: 6px; border-bottom: 1px solid var(--forest-line); overflow-wrap: anywhere; }
  li button { text-align: start; }
  pre { white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; }
</style>
