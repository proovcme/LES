<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { errorMessage } from './lib/api';
  import type { Source } from './lib/contracts';
  import { Workspace } from './lib/workspace.svelte';
  import { connectNativeClose } from './lib/native-close';
  import Icon from './components/Icon.svelte';
  import Modal from './components/Modal.svelte';
  import Answer from './components/Answer.svelte';
  import SourcePanel from './components/SourcePanel.svelte';
  import ScopePicker from './components/ScopePicker.svelte';
  import AgentMemory from './components/AgentMemory.svelte';

  let { workspace = new Workspace() }: { workspace?: Workspace } = $props();
  let modal = $state<'sessions' | 'scope' | 'options' | null>(null);
  let source = $state<Source | null>(null); let mobileNav = $state(false);
  let actionBusy = $state(false); let sessionQuery = $state('');
  let memoryOpen = $state(false); let memoryDirty = $state(false); let memoryEditor: AgentMemory;
  let composer: HTMLTextAreaElement; let transcript = $state<HTMLDivElement>(); let fileInput: HTMLInputElement;
  let sourceTrigger: HTMLElement | null = null; let followAnswer = $state(true);
  const disabled = $derived(workspace.busy || workspace.uploading || workspace.closing || actionBusy || workspace.loading);
  const scopeLabel = $derived(workspace.session?.scope.dataset_ids.length
    ? workspace.session.scope.dataset_ids.map((id) => workspace.datasets.find((item) => item.id === id)?.name || 'Набор недоступен').join(', ')
    : workspace.session?.scope.scope_type === 'all' ? 'Все документы' : 'Без набора документов');
  const sessions = $derived(workspace.sessions.filter((item) => item.title.toLocaleLowerCase('ru').includes(sessionQuery.toLocaleLowerCase('ru'))));
  const lastMessageText = $derived(workspace.messages.at(-1)?.text || '');

  onMount(() => {
    workspace.beforeClose = async () => { await memoryEditor?.flush(); };
    const bridge = connectNativeClose(workspace);
    void bridge.ready.then(() => workspace.initialize()).catch(() => {
      workspace.loading = false;
      workspace.error = 'Не удалось включить сохранение при закрытии окна. Перезапустите Лес; запуск чата остановлен, чтобы сохранить ваши данные.';
    });
    return () => { bridge.dispose(); workspace.dispose(); };
  });
  $effect(() => {
    lastMessageText;
    if (followAnswer) void tick().then(() => { if (transcript) transcript.scrollTop = transcript.scrollHeight; });
  });
  async function action(callback: () => Promise<void>) {
    if (actionBusy) return;
    actionBusy = true;
    try { await memoryEditor?.flush(); await callback(); modal = null; mobileNav = false; await tick(); composer?.focus(); }
    catch (error) { workspace.error = errorMessage(error); }
    finally { actionBusy = false; }
  }
  async function legacy(path: string) {
    if (!workspace.legacyURL || disabled) return;
    await action(async () => {
      await workspace.persist();
      const url = new URL(path, workspace.legacyURL!);
      location.assign(url);
    });
  }
  function openSource(value: Source) { sourceTrigger = document.activeElement as HTMLElement; source = value; }
  async function closeSource() { source = null; await tick(); (sourceTrigger?.isConnected ? sourceTrigger : composer)?.focus(); }
  function keyboard(event: KeyboardEvent) {
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing && !event.ctrlKey && !event.altKey && !event.metaKey) {
      event.preventDefault(); followAnswer = true; void workspace.send();
    }
  }
  function dropped(event: DragEvent) {
    event.preventDefault();
    const files = event.dataTransfer?.files;
    if (!files?.length) return;
    if (files.length > 1) { workspace.error = 'Прикрепляйте к вопросу по одному файлу. Для нескольких документов создайте набор.'; return; }
    void workspace.attach(files[0]);
  }
  function beforeLeave(event: BeforeUnloadEvent) {
    if (memoryDirty || workspace.busy || workspace.uploading || workspace.draftStatus.startsWith('Сохраняю') || workspace.draftStatus.startsWith('Черновик пока')) event.preventDefault();
  }
</script>

<svelte:window onbeforeunload={beforeLeave} />
<a class="skip-link" href="#chat-content">Перейти к диалогу</a>
<div class="app-shell">
  <nav class:mobile-open={mobileNav} aria-label="Основная навигация">
    <a class="brand" href="/app/" aria-label="Лес — главный экран"><Icon name="tree" size={30} /><span>ЛЕС<small>RAG</small></span></a>
    <button class="new-chat" disabled={disabled} onclick={() => action(() => workspace.newSession())}><Icon name="plus" />Новый диалог</button>
    <div class="nav-links">
      <button class="nav-item selected" aria-current="page" onclick={() => { mobileNav = false; composer?.focus(); }}><Icon name="chat" />Чат</button>
      <button class="nav-item" disabled={disabled} onclick={() => { modal = 'sessions'; mobileNav = false; }}><Icon name="history" />Диалоги</button>
      <button class="nav-item" disabled={disabled} onclick={() => { memoryOpen = true; mobileNav = false; }}><Icon name="file" />Память</button>
      {#if workspace.legacyURL}
        <button class="nav-item" disabled={disabled} onclick={() => legacy('/classic?tab=data')}><Icon name="folder" />Данные</button>
        <button class="nav-item" disabled={disabled} onclick={() => legacy('/qdrant-visualizer/index.html')}><Icon name="tree" />Лес знаний</button>
      {/if}
    </div>
    <div class="nav-footer">
      {#if workspace.legacyURL}<button class="nav-item" disabled={disabled} title="Модели, память и инструменты" onclick={() => legacy('/les/classic?tab=models')}><Icon name="settings" />Настройки</button>{/if}
      <p>В лесу нельзя заблудиться.</p>
    </div>
  </nav>
  <main>
    <header class="chat-header">
      <button class="mobile-menu icon-button quiet" aria-label={mobileNav ? 'Закрыть меню' : 'Открыть меню'} aria-expanded={mobileNav} onclick={() => mobileNav = !mobileNav}><Icon name={mobileNav ? 'close' : 'menu'} /></button>
      <div class="chat-heading"><h1>{workspace.session?.title || 'Новый диалог'}</h1></div>
      <div class="model-indicator" title="Назначенная модель ответов"><span>{workspace.model?.model_id || 'Модель не назначена'}<small>{workspace.service || 'Модель ответов'}{workspace.model?.status === 'blocked' ? ' · требует проверки' : ''}</small></span></div>
    </header>
    <div id="chat-content" class="chat-content" tabindex="-1">
      {#if workspace.notice}<p class="notice" role="status">{workspace.notice}</p>{/if}
      {#if workspace.loading}
        <div class="empty-state"><Icon name="tree" size={48} /><h2>Открываю ваши диалоги…</h2><p role="status">Загружаю историю и сохранённый черновик.</p></div>
      {:else}
        <div class="transcript" bind:this={transcript} onscroll={() => { if (transcript) followAnswer = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight < 100; }}>
          <div class="conversation">
            {#if !workspace.messages.length}
              <div class="empty-state"><Icon name="chat" size={28} />
                {#if workspace.session && !workspace.model && workspace.legacyURL}
                  <h2>Подключите модель</h2><p>Выберите локальную модель или API.<br />Затем добавьте документы и задайте вопрос.</p>
                  <button disabled={disabled} onclick={() => legacy('/les/classic?tab=models')}>Подключить модель</button>
                {:else}
                  <h2>Что найдём в ваших документах?</h2><p>Выберите набор или прикрепите файл.<br />Лес ответит и покажет источники.</p>
                  <button class="quiet" disabled={disabled || !workspace.session} onclick={() => modal = 'scope'}><Icon name="folder" />Выбрать документы</button>
                {/if}
              </div>
            {/if}
            {#each workspace.messages as message, index}
              <Answer {message} {openSource} streaming={workspace.busy && index === workspace.messages.length - 1} />
            {/each}
          </div>
        </div>
        {#if !followAnswer && workspace.busy}<button class="latest quiet" onclick={() => { followAnswer = true; if (transcript) transcript.scrollTop = transcript.scrollHeight; }}>К ответу ↓</button>{/if}
      {/if}
      <div class="composer-zone">
        {#if workspace.error}<div class="error" role="alert"><p>{workspace.error}</p>{#if !workspace.session}<button class="quiet" disabled={workspace.loading} onclick={() => location.reload()}>Повторить подключение</button>{/if}</div>{/if}
        {#if workspace.busy || workspace.uploading || workspace.closing || workspace.status}<div class="activity" role="status" aria-live="polite">{#if workspace.closing}Сохраняю диалог перед закрытием…{:else if workspace.busy}<span class="activity-dot"></span>{workspace.status} · {workspace.elapsed} с{:else if workspace.uploading}<span class="activity-dot"></span>Читаю файл…{:else}{workspace.status}{/if}</div>{/if}
        <form class="composer" onsubmit={(event) => { event.preventDefault(); followAnswer = true; void workspace.send(); }}>

          {#if workspace.attachment}<div class="attachment"><Icon name="file" /><span>{workspace.attachment.name}</span><button type="button" class="icon-button quiet" aria-label="Убрать вложение" disabled={workspace.busy} onclick={() => { workspace.attachment = null; workspace.changed(); }}><Icon name="close" size={16} /></button></div>{/if}
          <label for="question" class="sr-only">Ваш вопрос Лесу</label>
          <textarea id="question" bind:this={composer} bind:value={workspace.text} oninput={() => workspace.changed()} onkeydown={keyboard} ondragover={(event) => event.preventDefault()} ondrop={dropped} placeholder="Спросите Лес…" rows="2" maxlength="20000" disabled={workspace.loading || workspace.closing || !workspace.session} aria-describedby="composer-hint"></textarea>
          <div class="composer-actions"><input class="sr-only" type="file" bind:this={fileInput} tabindex="-1" aria-label="Выбрать файл для чтения" onchange={() => { const file = fileInput.files?.[0]; if (file) void workspace.attach(file); fileInput.value = ''; }} /><button type="button" class="icon-button quiet" aria-label="Прикрепить файл для чтения" disabled={disabled || !workspace.session} onclick={() => fileInput.click()}><Icon name="paperclip" /></button><button type="button" class="scope-button quiet" disabled={disabled || !workspace.session} onclick={() => modal = 'scope'} title={scopeLabel}><Icon name="folder" /><span>{scopeLabel}</span></button><button type="button" class="icon-button quiet options-button" aria-label="Параметры ответа" aria-pressed={workspace.reranker} disabled={disabled} onclick={() => modal = 'options'}><Icon name="settings" /></button>
            {#if workspace.busy}<button type="button" class="stop-button" aria-label="Остановить ответ" onclick={() => workspace.stop()}><Icon name="stop" /><span class="send-label">Остановить</span></button>{:else}<button type="submit" class="send-button" disabled={disabled || !workspace.session || !workspace.text.trim()} aria-label="Отправить вопрос"><Icon name="arrow" /><span class="send-label">Отправить</span></button>{/if}
          </div>
        </form>
        <div class="composer-footnote"><p class="draft-status" aria-live="polite">{workspace.draftStatus}</p><span id="composer-hint">Shift + Enter — новая строка</span></div>
      </div>
    </div>
  </main>
</div>
<AgentMemory bind:this={memoryEditor} bind:open={memoryOpen} projectId={workspace.session?.project_id || 0} onDirty={(value) => memoryDirty = value} />
{#if modal === 'sessions'}
  <Modal title="Ваши диалоги" close={() => modal = null}>
    <label for="session-search">Найти диалог</label><input id="session-search" type="search" bind:value={sessionQuery} placeholder="Название или начало вопроса" />
    <ul class="session-list">{#each sessions as session (session.session_id)}<li><button class="quiet session-button" disabled={actionBusy} aria-current={workspace.session?.session_id === session.session_id ? 'true' : undefined} onclick={() => action(() => workspace.openSession(session.session_id))}><span>{session.title || 'Новый диалог'}</span><small>{new Date(session.updated_at).toLocaleDateString('ru-RU')}</small></button></li>{:else}<li class="muted">Диалогов по этому запросу нет.</li>{/each}</ul>
    {#if workspace.error}<p class="error" role="alert">{workspace.error}</p>{/if}
  </Modal>
{:else if modal === 'scope' && workspace.session}
  <ScopePicker scope={workspace.session.scope} datasets={workspace.datasets} save={(scope) => workspace.setScope(scope)} close={() => modal = null} />
{:else if modal === 'options'}
  <Modal title="Параметры ответа" close={() => modal = null}>
    <label class="option"><input type="checkbox" bind:checked={workspace.reranker} /><span><strong>Уточнять результаты поиска</strong><small>Дополнительное ранжирование найденных фрагментов. Может увеличить время ответа.</small></span></label>
    <div class="modal-footer"><button onclick={() => modal = null}>Готово</button></div>
  </Modal>
{/if}
{#if source}{#key source.index}<SourcePanel {source} close={closeSource} />{/key}{/if}
{#if workspace.draftConflict}
  <Modal title="Два черновика одного диалога" close={() => workspace.draftConflict = null}>
    <p>В другом окне сохранён другой текст. Выберите, какую версию оставить.</p>
    <label for="local-draft">В этом окне</label><textarea id="local-draft" value={workspace.text} readonly rows="4"></textarea>
    <label for="remote-draft">Сохранённая версия</label><textarea id="remote-draft" value={workspace.draftConflict.text} readonly rows="4"></textarea>
    <div class="modal-footer"><button class="quiet" onclick={() => action(() => workspace.resolveDraft(false))}>Загрузить сохранённую</button><button onclick={() => action(() => workspace.resolveDraft(true))}>Оставить мою</button></div>
  </Modal>
{/if}
