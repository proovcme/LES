<script lang="ts">
  import type { Message, Source } from '../lib/contracts';
  import { renderAnswer, citations } from '../lib/markdown';
  import SourceButton from './SourceButton.svelte';
  import RequestContext from './RequestContext.svelte';
  let { message, openSource, streaming = false, sessionId, excluded, contextDisabled, toggleContext }: {
    message: Message; openSource: (source: Source) => void; streaming?: boolean; sessionId?: string;
    excluded?: boolean; contextDisabled?: boolean; toggleContext?: () => Promise<void>;
  } = $props();
  const sources = $derived(message.meta?.source_map || []);
  const rendered = $derived(renderAnswer(message.text, sources));
  const quoted = $derived(sources.filter((source) => rendered.linked.has(source.index)));
  const found = $derived(sources.filter((source) => !rendered.linked.has(source.index)));
  const citationStatus = $derived(message.meta?.retrieval_trace?.citation_check?.status);
  const missingAttribution = $derived(!streaming && message.text
    && (citationStatus === 'missing_labels' || citationStatus === 'no_sources'
      || (sources.length > 0 && quoted.length === 0)));
  const execution = $derived(message.meta?.execution ?? message.meta?.retrieval_trace?.execution);
</script>
<article class:question={message.role === 'user'} class="message" aria-label={message.role === 'user' ? 'Ваш вопрос' : 'Ответ Леса'}>
  <p class="message-byline">{message.role === 'user' ? 'Вы' : 'Лес'}{#if excluded}<span class="context-marker"> · Вне контекста</span>{/if}</p>
  {#if message.role === 'user'}
    <div class="question-text">{message.text}</div>
  {:else}
    {#if execution}<p class={execution.status === 'incomplete' ? 'error' : execution.status === 'no_changes' ? 'muted' : 'notice'} role="status">{execution.notice}</p>{/if}
    {#if message.meta?.retrieval_trace?.tool_loop?.status === 'error'}
      <p class="error" role="status">Работа с инструментами прервалась. Текст модели не подтверждает выполнение всех действий. Проверьте подключение модели и повторите запрос.</p>
    {/if}
    {#if message.meta?.retrieval_trace?.status === 'blocked' || message.meta?.total_status === 'blocked' || message.crag === 'BLOCKED'}
      <p class="error" role="status">{message.meta?.blocker?.action || 'Поиск заблокирован. Проверьте состояние набора документов и повторите запрос.'}</p>
    {:else if message.meta?.retrieval_trace?.status === 'degraded'}
      <p class="notice" role="status">Поиск работает с ограничениями: часть поискового контура недоступна. Проверьте источники перед использованием ответа.</p>
    {/if}
    {#if missingAttribution}
      <p class="notice" role="status">У вывода нет ссылки на фрагмент документа.</p>
    {/if}
    <div class="answer-text" use:citations={(index) => { const source = sources.find((item) => item.index === index); if (source) openSource(source); }}>{@html rendered.html}</div>
    {#if !missingAttribution && ['invalid_labels', 'error'].includes(citationStatus || '')}
      <p class="notice" role="status">Часть ссылок в ответе не подтверждена.</p>
    {/if}
    {#if message.meta?.error}<p class="partial" role="alert">{message.meta.error.detail}</p>
    {:else if message.meta?.partial}<p class="partial">{message.meta.completion_status === 'in_progress' ? 'Ответ ещё формируется.' : (message.text ? 'Ответ прерван. Полученный текст сохранён; проверьте его перед использованием.' : 'Ответ прерван до получения текста. Вопрос можно повторить.')}</p>
    {:else if !message.text && !streaming}<p class="muted">Текст ответа не получен. Вопрос можно повторить.</p>{/if}
    {#if quoted.length}
      <section class="sources" aria-label="Цитаты в ответе">
        <p class="sources-title">Цитаты в ответе</p>
        <ul>{#each quoted.slice(0, 3) as source (source.index)}<li><SourceButton {source} open={openSource} /></li>{/each}</ul>
        {#if quoted.length > 3}<details><summary>Ещё цитаты · {quoted.length - 3}</summary><ul>{#each quoted.slice(3) as source (source.index)}<li><SourceButton {source} open={openSource} /></li>{/each}</ul></details>{/if}
      </section>
    {/if}
    {#if found.length}
      <section class="sources" aria-label="Найденные материалы">
        <details open={!quoted.length && !streaming}>
          <summary>Найденные материалы · {found.length}</summary>
          <ul>{#each found as source (source.index)}<li><SourceButton {source} open={openSource} /></li>{/each}</ul>
        </details>
      </section>
    {:else if !sources.length && message.srcs?.length}
      <p class="muted">У ответа нет привязки к фрагментам. Упоминания документов: {message.srcs.join(', ')}.</p>
    {/if}
  {/if}
  {#if message.history_id && !streaming && (sessionId && message.role === 'ai' || toggleContext)}
    <footer class="message-tools">
      {#if sessionId && message.role === 'ai'}
        <RequestContext {sessionId} historyId={message.history_id} {openSource} />
      {/if}
      {#if toggleContext}
        <details class="message-actions">
          <summary>Действия<span class="sr-only"> · {message.role === 'user' ? 'вопрос' : 'ответ'}</span></summary>
          <button class="quiet" disabled={contextDisabled} onclick={(event) => {
            const menu = event.currentTarget.closest('details');
            if (menu) { menu.open = false; menu.querySelector('summary')?.focus(); }
            void toggleContext?.();
          }}>{excluded ? 'Вернуть в контекст' : 'Исключить из контекста'}</button>
        </details>
      {/if}
    </footer>
  {/if}
</article>
