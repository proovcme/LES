<script lang="ts">
  import { onMount } from 'svelte';
  import type { Snippet } from 'svelte';
  import { errorMessage, request } from '../lib/api';
  import { activeJob, jobProgress, jobStage, searchStatus, visibleIndexingJobs } from '../lib/system-status';
  import type { Health, StatusJob, StatusModels } from '../lib/system-status';

  let { jobs = $bindable(null), actions, showIndexHealth = false }: {
    jobs?: StatusJob[] | null; actions?: Snippet; showIndexHealth?: boolean;
  } = $props();
  const visible = $derived(visibleIndexingJobs(jobs ?? []));
  let health = $state<Health | null>(null);
  let models = $state<StatusModels | null>(null);
  const search = $derived(searchStatus(health, models));
  const number = new Intl.NumberFormat('ru-RU');
  let retrying = $state<string | null>(null);
  let retryError = $state('');
  let refreshError = $state(false);
  let pending: Promise<void> | null = null;
  let alive = true;

  export function refresh() {
    if (pending) return pending;
    const summary = request<{ jobs: StatusJob[] }>('/api/jobs/summary?limit=30').catch(() => null);
    const index = showIndexHealth ? request<Health>('/api/health').catch(() => null) : null;
    const connections = showIndexHealth
      ? request<StatusModels>('/api/model-connections/effective').catch(() => null) : null;
    pending = Promise.all([summary, index, connections]).then(([result, indexHealth, effectiveModels]) => {
      if (!alive) return;
      if (result) jobs = result.jobs;
      refreshError = !result;
      health = indexHealth;
      models = effectiveModels;
    }).finally(() => { pending = null; });
    return pending;
  }
  async function retryIndexing(id: string) {
    if (retrying) return;
    retrying = id;
    retryError = '';
    try {
      const job = await request<StatusJob>('/api/light/intake', {
        method: 'POST', body: JSON.stringify({ id }),
      });
      await pending;
      if (alive && jobs) jobs = jobs.map((item) => item.id === id ? job : item);
      if (alive) await refresh();
    } catch (error) {
      if (alive) retryError = errorMessage(error);
    } finally {
      retrying = null;
    }
  }
  onMount(() => {
    void refresh();
    const timer = setInterval(() => { if (!document.hidden) void refresh(); }, 6000);
    return () => { alive = false; clearInterval(timer); };
  });
</script>

<section class="status-jobs" aria-label="Обработка документов">
  <div class="section-heading"><h3>Обработка документов</h3>{@render actions?.()}</div>
  {#if showIndexHealth}
    <p><strong>Индекс · {search.label}</strong>{#if search.tone !== 'ready'} <span class="muted">{search.detail}</span>{/if}</p>
  {/if}
  {#if retryError}<p class="error" role="alert">{retryError}</p>{/if}
  {#if refreshError}<p class="muted" role="status">{jobs ? 'Не удалось обновить прогресс. Показаны последние данные.' : 'Очередь обработки недоступна.'}</p>{/if}
  {#if !jobs && !refreshError}
    <p class="muted" role="status">Загружаю очередь…</p>
  {:else if jobs && !visible.length}
    <p class="muted">Нет заданий по индексации.</p>
  {:else if visible.length}
    <ul class="job-list">
      {#each visible as job (job.id)}
        {@const progress = jobProgress(job)}
        {@const indexing = job.phase === 'index'}
        <li>
          <strong>{job.dataset_name || (job.type === 'dataset_intake' ? 'Импорт папки' : 'Задание Леса')}</strong>
          <p>{jobStage(job)}</p>
          {#if activeJob(job) && job.current_file}<small class="current-file">{job.current_file.split(/[\\/]/).at(-1)}</small>{/if}
          {#if progress.total > 0}
            {#if activeJob(job)}<progress max={progress.total} value={progress.completed} aria-label="Обработано файлов"></progress>{/if}
            <div class="counts">
              {#if indexing}
                <span>В поиске <strong>{number.format(job.indexed ?? 0)} из {number.format(progress.total)}</strong></span>
                {#if (job.index_pending ?? 0) > 0}<span>Осталось {number.format(job.index_pending!)}</span>{/if}
                {#if (job.index_errors ?? 0) > 0}<span>Ошибки {number.format(job.index_errors!)}</span>{/if}
                {#if (job.index_skipped ?? 0) > 0}<span>Пропущено {number.format(job.index_skipped!)}</span>{/if}
              {:else}
                <span>Зарегистрировано {number.format(progress.completed)} из {number.format(progress.total)}</span>
              {/if}
            </div>
          {/if}
          {#if job.can_retry === 1}
            <button class="quiet retry" type="button" disabled={retrying !== null} onclick={() => retryIndexing(job.id)}>
              {retrying === job.id ? 'Возобновляю…' : 'Повторить индексацию'}
            </button>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .status-jobs { margin-block: 20px; overflow-wrap: anywhere; }
  .section-heading { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 4px 16px; }
  h3 { margin: 0; font-size: 18px; }
  p { margin: 4px 0 0; font-size: 14px; }
  .job-list { list-style: none; margin: 10px 0 0; padding: 0; background: var(--forest-paper); border: 1px solid var(--forest-line); border-radius: 12px; }
  .job-list li { padding: 12px 14px; }
  .job-list li + li { border-top: 1px solid var(--forest-line); }
  .current-file { margin-top: 4px; }
  progress { display: block; width: 100%; height: 9px; margin-top: 10px; accent-color: var(--forest-green); }
  .counts { display: flex; flex-wrap: wrap; gap: 4px 16px; margin-top: 6px; font-size: 13px; color: var(--forest-muted); font-variant-numeric: tabular-nums; }
  .counts strong { color: var(--forest-ink); }
  .retry { margin-top: 8px; }
</style>
