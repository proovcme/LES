<script lang="ts">
  import { errorMessage, request } from '../lib/api';
  import { contextPath, type ContextReportCall, type Source } from '../lib/contracts';
  import SourceButton from './SourceButton.svelte';
  let { sessionId, historyId, openSource }: {
    sessionId: string; historyId: number; openSource: (source: Source) => void;
  } = $props();

  let calls = $state<ContextReportCall[] | null>(null);
  let selected = $state(0);
  let loading = $state(false);
  let error = $state('');
  const call = $derived(calls?.[selected]);
  const number = new Intl.NumberFormat('ru-RU');
  const sectionNames: Record<string, string> = {
    profile_prefix: 'Инструкции и профиль', request: 'Ваш запрос',
    evidence: 'Документы и цитаты', source_map: 'Список ссылок',
    tool_shortlist: 'Доступные инструменты', tool_exchange: 'Результаты инструментов',
    native_tool_exchange: 'Вызовы и результаты инструментов',
    checkpoint: 'Сводка диалога', working_memory: 'Рабочая память', dialogue: 'История диалога',
  };
  const callName = (item: ContextReportCall) => item.produced_answer || item.purpose === 'answer'
    ? 'Ответ' : 'Выбор инструментов';

  async function load() {
    if (loading || calls) return;
    loading = true; error = '';
    try {
      const report = await request<{ calls: ContextReportCall[] }>(`${contextPath(sessionId)}/report?history_id=${historyId}`);
      calls = report.calls;
      const answer = calls.map((item) => item.produced_answer).lastIndexOf(true);
      const delivered = calls.map((item) => item.delivery !== 'prepared').lastIndexOf(true);
      selected = Math.max(0, answer >= 0 ? answer : delivered);
    } catch (failure) { error = errorMessage(failure); }
    finally { loading = false; }
  }
</script>

<details class="request-context" ontoggle={(event) => { if (event.currentTarget.open) void load(); }}>
  <summary>Контекст запроса</summary>
  <div class="context-body">
    {#if loading}
      <p class="muted" role="status">Загружаю сведения о запросе…</p>
    {:else if error}
      <p class="error" role="status">{error}</p>
      <button class="quiet" onclick={load}>Повторить</button>
    {:else if call}
      {#if calls && calls.length > 1}
        <label class="context-call">Вызов модели
          <select bind:value={selected}>
            {#each calls as item, i}<option value={i}>{i + 1}. {callName(item)}{item.delivery === 'prepared' ? ' · подготовлен' : ''}</option>{/each}
          </select>
        </label>
      {/if}
      <p class="context-state muted">
        {call.delivery === 'confirmed' ? 'Модель приняла запрос'
          : call.delivery === 'legacy' ? 'Отправка отмечена в журнале предыдущей сборки'
          : 'Пакет подготовлен. Отправка не подтверждена.'}
      </p>
      <div class="context-budget">
        <strong>{number.format(call.used)} / {number.format(call.budget)} токенов</strong>
        <span class="muted">Оценка Леса</span>
      </div>
      {#if call.budget > 0}<meter aria-label="Использовано из бюджета входного контекста" min="0" max={call.budget} value={call.used}></meter>{/if}
      <dl class="context-breakdown">
        {#each call.sections as section}
          <div><dt>{sectionNames[section.kind]}</dt><dd>{number.format(section.tokens)}</dd></div>
        {/each}
        <div><dt>Свободно во входном бюджете</dt><dd>{number.format(call.free)}</dd></div>
        {#if call.generation_reserve !== null}<div><dt>Резерв для ответа</dt><dd>{number.format(call.generation_reserve)}</dd></div>{/if}
        {#if call.safety_reserve}<div><dt>Технический запас</dt><dd>{number.format(call.safety_reserve)}</dd></div>{/if}
        {#if call.provider_prompt_tokens !== null}<div><dt>Входных токенов по данным модели</dt><dd>{number.format(call.provider_prompt_tokens)}</dd></div>{/if}
      </dl>
      {#if call.sources.length}
        <p class="sources-title">{call.delivery === 'prepared' ? 'Источники в подготовленном пакете' : 'Источники в запросе'}</p>
        <ul class="context-sources">
          {#each call.sources as source (source.index)}
            <li>
              <SourceButton source={{ ...source, snippet: source.model_excerpt }} open={openSource} />
              {#if source.model_excerpt}
                <details class="context-excerpt">
                  <summary>Текст фрагмента</summary>
                  <p>{source.model_excerpt}</p>
                  {#if source.excerpt_limited}<p class="muted">Показано начало длинного фрагмента.</p>{/if}
                </details>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      {#if call.omissions.length || call.omitted_sources.length}
        <p class="sources-title">Не поместилось в бюджет</p>
        <ul class="context-omissions">
          {#each call.omissions as item}<li>{sectionNames[item.kind]} · {number.format(item.count)}</li>{/each}
          {#each call.omitted_sources as source}<li>{source.doc_name || `Источник ${source.index}`}</li>{/each}
        </ul>
      {/if}
    {:else if calls}
      <p class="muted">Для этого ответа нет записи о запросе к модели.</p>
    {/if}
  </div>
</details>
