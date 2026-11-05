<script lang="ts">
  import type { Message, Source } from '../lib/contracts';
  import { answerHTML, citations } from '../lib/markdown';
  import SourceButton from './SourceButton.svelte';
  let { message, openSource, streaming = false }: { message: Message; openSource: (source: Source) => void; streaming?: boolean } = $props();
  const sources = $derived(message.meta?.source_map || []);
  const html = $derived(answerHTML(message.text, sources));
  const execution = $derived(message.meta?.execution ?? message.meta?.retrieval_trace?.execution);
</script>
<article class:question={message.role === 'user'} class="message" aria-label={message.role === 'user' ? 'Ваш вопрос' : 'Ответ Леса'}>
  <p class="message-byline">{message.role === 'user' ? 'Вы' : 'Лес'}</p>
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
    <div class="answer-text" use:citations={(index) => { const source = sources.find((item) => item.index === index); if (source) openSource(source); }}>{@html html}</div>
    {#if ['missing_labels', 'invalid_labels', 'error'].includes(message.meta?.retrieval_trace?.citation_check?.status || '')}
      <p class="notice" role="status">Часть ссылок в ответе не подтверждена. Сверьте утверждения с фрагментами источников ниже.</p>
    {/if}
    {#if !message.text && !streaming}<p class="muted">Текст ответа не получен. Вопрос можно повторить.</p>{/if}
    {#if message.meta?.partial}<p class="partial">Ответ прерван. Полученный текст сохранён; проверьте его перед использованием.</p>{/if}
    {#if sources.length}
      <section class="sources" aria-label="Источники ответа">
        <p class="sources-title">Источники ответа</p>
        <ul>{#each sources.slice(0, 3) as source (source.index)}<li><SourceButton {source} open={openSource} /></li>{/each}</ul>
        {#if sources.length > 3}<details><summary>Ещё источники · {sources.length - 3}</summary><ul>{#each sources.slice(3) as source (source.index)}<li><SourceButton {source} open={openSource} /></li>{/each}</ul></details>{/if}
      </section>
    {:else if message.srcs?.length}
      <p class="muted">У ответа нет привязки к фрагментам. Упоминания документов: {message.srcs.join(', ')}.</p>
    {/if}
  {/if}
</article>
