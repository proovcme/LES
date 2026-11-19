<script lang="ts">
  import { onMount } from 'svelte';
  import { request } from '../lib/api';
  import { activeJob, chatStatus, failedJob, searchStatus, visibleIndexingJobs } from '../lib/system-status';
  import type { Health, RuntimeStatus, StatusJob, StatusModels } from '../lib/system-status';
  import Icon from './Icon.svelte';
  import IndexingJobs from './IndexingJobs.svelte';

  let health = $state<Health | null>(null);
  let models = $state<StatusModels | null>(null);
  let runtime = $state<RuntimeStatus | null>(null);
  let jobs = $state<StatusJob[] | null>(null);
  let legacy = $state<string | null>(null);
  let loading = $state(true);
  let queue: IndexingJobs;
  let updated = $state<Date | null>(null);
  const chat = $derived(chatStatus(models, runtime));
  const search = $derived(searchStatus(health, models));
  const visibleJobs = $derived(visibleIndexingJobs(jobs ?? []));
  const processing = $derived(visibleJobs.some((job) => activeJob(job) && job.status !== 'waiting'));
  const waiting = $derived(visibleJobs.some((job) => job.status === 'waiting'));
  const needsAttention = $derived(chat.tone === 'attention' || search.tone === 'attention'
    || visibleJobs.some(failedJob));
  const unknown = $derived(chat.tone === 'unknown' || search.tone === 'unknown' || !jobs);
  const title = $derived(loading && !updated ? 'Проверяю состояние'
    : processing ? 'Идёт обработка документов' : waiting ? 'Обработка ждёт продолжения'
    : needsAttention ? 'Требуется внимание' : unknown ? 'Состояние проверено частично' : 'Лес готов к работе');
  const time = new Intl.DateTimeFormat('ru-RU', { hour: '2-digit', minute: '2-digit' });
  const number = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 1 });
  const session = new URLSearchParams(location.search).get('session');
  const panelHref = (panel?: string) => {
    const params = new URLSearchParams();
    if (session) params.set('session', session);
    if (panel) params.set('panel', panel);
    return params.size ? `/app/?${params}` : '/app/';
  };
  const chatHref = panelHref();
  const legacyLink = (path: string) => legacy ? new URL(path, legacy).href : null;
  const modelLink = $derived(legacyLink('/les/classic?tab=models') ?? panelHref('models'));
  const dataLink = panelHref('data');
  const diagnostics = $derived(legacyLink('/les/classic?tab=state&details=1'));
  let alive = true;

  const check = <T,>(path: string) => request<T>(path).catch(() => null);
  async function refresh() {
    loading = true;
    const [service, connections, admission] = await Promise.all([
      check<Health>('/api/health'), check<StatusModels>('/api/model-connections/effective'),
      check<RuntimeStatus>('/api/status'), queue?.refresh(),
    ]);
    if (!alive) return;
    health = service; models = connections; runtime = admission;
    updated = new Date(); loading = false;
  }
  onMount(() => {
    void check<{ legacy_url: string | null }>('/api/light/frontend').then((value) => {
      if (alive) legacy = value?.legacy_url ?? null;
    });
    void refresh();
    return () => { alive = false; };
  });
</script>

<svelte:head><title>Состояние · Лес</title></svelte:head>
<a class="skip-link" href="#status-main">Перейти к состоянию</a>
<header class="status-header">
  <div class="status-header-content">
    <a class="brand" href={chatHref} aria-label="Лес · к чату"><Icon name="tree" size={30} /><span>ЛЕС<small>RAG</small></span></a>
    <div class="status-actions">
      <a class="button quiet" href={chatHref}><Icon name="back" size={16} />К чату</a>
      {#if modelLink}<a class="button quiet" href={modelLink}>Настройки</a>{/if}
    </div>
  </div>
</header>
<main class="status-main" id="status-main" tabindex="-1">
  <div class="status-intro">
    <h1>{title}</h1>
    <button class="quiet" disabled={loading} onclick={refresh}>{loading ? 'Проверяю…' : 'Обновить'}</button>
  </div>
  {#if updated}<p class="muted status-update">Службы проверены в {time.format(updated)}</p>{/if}
  <section class="status-surface" aria-label="Готовность к работе" aria-busy={loading}>
    {#each [{ name: 'Чат', row: chat }, { name: 'Поиск по документам', row: search }] as { name, row }}
      <div class="status-row">
        <div><h2>{name}</h2><p class="muted">{row.detail}</p></div>
        <div class="status-row-actions">
          <span class="status-label" data-tone={row.tone}>{row.label}</span>
          {#if row.action === 'models' && modelLink}<a class="button quiet" href={modelLink}>Модели</a>
          {:else if row.action === 'data'}<a class="button quiet" href={dataLink}>Данные</a>{/if}
        </div>
      </div>
    {/each}
  </section>
  <IndexingJobs bind:this={queue} bind:jobs>
    {#snippet actions()}<a href={dataLink}>Открыть данные</a>{/snippet}
  </IndexingJobs>
  <details class="status-diagnostics">
    <summary>Диагностика</summary>
    <div class="status-diagnostics-body">
      {#if runtime?.chat_admission?.memory?.ram_free_gb != null}
        <p>Свободно {number.format(runtime.chat_admission.memory.ram_free_gb)} ГБ оперативной памяти.</p>
      {/if}
      {#if health?.rag?.qdrant?.evidence_points != null}
        <p>Поисковых фрагментов: {number.format(health.rag.qdrant.evidence_points)}.</p>
      {:else if health?.rag?.qdrant?.points != null}
        <p>Точек в индексе: {number.format(health.rag.qdrant.points)}.</p>
      {/if}
      {#if diagnostics}<a class="button quiet" href={diagnostics}>Расширенная диагностика</a>{/if}
    </div>
  </details>
</main>

<style>
  .status-header { background:var(--forest-paper); border-bottom:1px solid var(--forest-line); }
  .status-header-content { display:flex; justify-content:space-between; gap:12px; flex-wrap:wrap; align-items:center; padding-block:10px; }
  .status-header .brand { padding-inline:0; }
  .status-actions, .status-row-actions { display:flex; flex-wrap:wrap; align-items:center; gap:8px; }
  .status-main, .status-header-content { max-width:56rem; margin-inline:auto; padding-inline:clamp(12px, 4vw, 32px); }
  .status-main { padding-block:clamp(18px, 4vw, 48px); overflow-wrap:anywhere; }
  .status-intro { display:flex; justify-content:space-between; flex-wrap:wrap; align-items:baseline; gap:10px 16px; }
  .status-intro h1 { margin:0; font-size:clamp(24px, 4vw, 38px); line-height:1.14; letter-spacing:-.04em; }
  .status-update { margin:8px 0 0; }
  .status-surface { background:var(--forest-paper); border:1px solid var(--forest-line); border-radius:14px; margin:18px 0 0; }
  .status-row { display:flex; align-items:center; justify-content:space-between; flex-wrap:wrap; gap:10px 18px; padding:14px clamp(12px, 3vw, 20px); }
  .status-row + .status-row { border-top:1px solid var(--forest-line); }
  .status-row h2 { font-size:16px; margin:0 0 4px; }
  .status-row p { margin:0; }
  .status-label { padding:4px 8px; border-radius:7px; background:var(--forest-floor); font-size:13px; }
  .status-label[data-tone="ready"] { color:var(--forest-green); background:var(--forest-selected); }
  .status-label[data-tone="attention"] { color:#6f501d; background:var(--forest-warning); }
  .status-diagnostics { margin-top:18px; border-top:1px solid var(--forest-line); }
  .status-diagnostics-body { padding:0 2px 8px; font-size:14px; }
</style>
