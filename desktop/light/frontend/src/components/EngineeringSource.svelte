<script lang="ts">
  import { ApiError, errorMessage } from '../lib/api';
  import UvyazkaInstall from './UvyazkaInstall.svelte';
  import { companionAddress, engineering, rememberCompanion } from '../lib/engineering';
  import type { ModelLocator } from '../lib/engineering';
  let { target }: { target: ModelLocator } = $props();
  let url = $state(companionAddress()); let link = $state(''); let busy = $state(false); let failure = $state(''); let reason = $state('');
  async function resolve() {
    if (busy) return; busy = true; failure = ''; reason = ''; link = '';
    try {
      const { project_id, dataset_id, object_id, source_sha256 } = target;
      const result = await engineering<{ url: string }>(url, 'viewer', { target: { project_id, dataset_id, object_id, source_sha256 } });
      link = result.url; rememberCompanion(url);
    } catch (e) { failure = errorMessage(e); reason = e instanceof ApiError ? e.code : ''; } finally { busy = false; }
  }
</script>
<section aria-label="Объект Увязки">
  <h3>{target.source_name}</h3>
  <p>ID объекта: <strong>{target.native_id}</strong> · данные Увязки</p>
  <p class="muted">Это сохранённые свойства, не утверждённое инженерное решение. Полнота: {target.coverage.status === 'complete' ? 'заявлена читателем' : 'частичная'}.</p>
  {#if target.coverage.limitations?.length}<ul>{#each target.coverage.limitations as limit}<li>{limit}</li>{/each}</ul>{/if}
  <form onsubmit={(event) => { event.preventDefault(); void resolve(); }}>
    <label for="source-uvyazka-address">Локальный адрес Увязки</label><input id="source-uvyazka-address" type="url" bind:value={url} required disabled={busy} oninput={() => link = ''} />
    <div class="source-actions"><button class="quiet" disabled={busy || !url.trim()}>{busy ? 'Проверяю редакцию…' : 'Открыть объект во вьювере'}</button>
      {#if link}<a class="button" href={link} target="_blank" rel="noopener noreferrer">Показать в Увязке ↗</a>{/if}</div>
  </form>
  {#if failure}<p class="error" role="alert">{failure}</p>{/if}
  {#if reason === 'ENGINEERING_UNAVAILABLE' || reason === 'ENGINEERING_VIEWER_UPDATE_REQUIRED'}<UvyazkaInstall {reason} />{/if}
</section>
