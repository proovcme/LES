<script lang="ts">
  import { untrack } from 'svelte';
  import type { Dataset, Scope } from '../lib/contracts';
  import { errorMessage, request } from '../lib/api';
  import Modal from './Modal.svelte';
  let { scope, datasets, save, close }: { scope: Scope; datasets: Dataset[]; save: (value: Scope) => Promise<void>; close: () => void } = $props();
  const keepCurrent = $derived(scope.project_ids.length > 0 || scope.selected_sources_only);
  let kind = $state(untrack(() => scope.document_ids?.length ? 'documents' : keepCurrent ? 'current' : scope.dataset_ids.length ? 'datasets' : scope.scope_type === 'all' ? 'all' : 'none'));
  let selected = $state(untrack(() => [...scope.dataset_ids])); let saving = $state(false); let error = $state('');
  type Doc = { id: string; dataset_id: string; file_name: string };
  let pinned = $state<string[]>(untrack(() => [...(scope.document_ids ?? [])]));
  let known = $state<Record<string, Doc>>({});
  let dataset = $state(untrack(() => scope.dataset_ids[0] || datasets[0]?.id || ''));
  let query = $state(''); let offset = $state(0);
  let page = $state<{ documents: Doc[]; total: number }>({ documents: [], total: 0 });
  let loading = $state(false); let listError = $state('');
  $effect(() => {
    if (kind !== 'documents' || !dataset) return;
    const params = new URLSearchParams({ dataset_id: dataset, q: query.trim(), offset: String(offset), limit: '25' });
    const abort = new AbortController();
    loading = true; listError = ''; page = { documents: [], total: 0 };
    request<typeof page>(`/api/light/library?${params}`, { signal: abort.signal }).then((value) => {
      if (abort.signal.aborted) return;
      page = value;
      for (const doc of value.documents) known[doc.id] = doc;
    }).catch((failure) => { if (!abort.signal.aborted) listError = errorMessage(failure); })
      .finally(() => { if (!abort.signal.aborted) loading = false; });
    return () => abort.abort();
  });
  function toggleDoc(id: string) {
    pinned = pinned.includes(id) ? pinned.filter((value) => value !== id) : [...pinned, id];
  }
  async function submit(event: SubmitEvent) {
    event.preventDefault(); saving = true; error = '';
    try {
      if (kind === 'current') { close(); return; }
      if (kind === 'documents') {
        if (!pinned.length) { error = 'Выберите хотя бы один документ.'; return; }
        const ids = new Set(pinned.flatMap((id) => known[id] ? [known[id].dataset_id] : scope.dataset_ids));
        await save({ scope_type: 'datasets', dataset_ids: [...ids], project_ids: [], selected_sources_only: false, document_ids: pinned });
        close(); return;
      }
      if (kind === 'datasets' && !selected.length) { error = 'Выберите хотя бы один набор.'; return; }
      await save({ scope_type: kind as Scope['scope_type'], dataset_ids: kind === 'datasets' ? selected : [], project_ids: [], selected_sources_only: false });
      close();
    } catch (failure) { error = errorMessage(failure); }
    finally { saving = false; }
  }
</script>
<Modal title="Где искать ответ" {close}>
  <form onsubmit={submit}>
    <fieldset disabled={saving}><legend class="sr-only">Область поиска</legend>
      {#if keepCurrent}
        <label class="option"><input type="radio" bind:group={kind} value="current" /><span><strong>Текущая область</strong><small>Сохранить выбранные источники и документы проекта.</small></span></label>
      {/if}
      <label class="option"><input type="radio" bind:group={kind} value="documents" /><span><strong>Конкретные документы</strong><small>Перед ответом Лес прочитает фрагмент каждого файла. Дополнительный поиск возможен в тех же наборах.</small></span></label>
      {#if kind === 'documents'}
        <label>Набор <select bind:value={dataset} onchange={() => { offset = 0; }}>{#each datasets as item (item.id)}<option value={item.id}>{item.name}</option>{/each}</select></label>
        <label>Имя или путь <input type="search" bind:value={query} oninput={() => { offset = 0; }} /></label>
        {#if listError}<p role="alert">{listError}</p>{:else if loading}<p role="status">Загружаю документы…</p>
        {:else}<div class="dataset-options">{#each page.documents as doc (doc.id)}
          <label class="option"><input type="checkbox" checked={pinned.includes(doc.id)} onchange={() => toggleDoc(doc.id)} /><span>{doc.file_name}</span></label>
        {:else}<p>Документы не найдены.</p>{/each}</div>{/if}
        {#if offset > 0 || offset + 25 < page.total}
          <button type="button" class="quiet" disabled={loading || offset === 0} onclick={() => { offset -= 25; }}>Назад</button>
          <button type="button" class="quiet" disabled={loading || offset + 25 >= page.total} onclick={() => { offset += 25; }}>Дальше</button>
        {/if}
        {#if pinned.length}<p>Выбрано документов: {pinned.length} <button type="button" class="quiet" onclick={() => { pinned = []; }}>Снять выбор</button></p>{/if}
      {/if}
      <label class="option"><input type="radio" bind:group={kind} value="none" /><span><strong>Без набора документов</strong><small>Обычный диалог или чтение прикреплённого файла.</small></span></label>
      <label class="option"><input type="radio" bind:group={kind} value="all" /><span><strong>Все подключённые документы</strong><small>Лес найдёт подходящие источники сам.</small></span></label>
      <label class="option"><input type="radio" bind:group={kind} value="datasets" /><span><strong>Выбранные наборы</strong><small>Ограничить поиск документами по задаче.</small></span></label>
      {#if kind === 'datasets'}
        <div class="dataset-options">{#each datasets as dataset (dataset.id)}
          <label class="option"><input type="checkbox" bind:group={selected} value={dataset.id} /><span>{dataset.name}{#if (dataset.document_count ?? dataset.documents) != null}<small>Документов: {dataset.document_count ?? dataset.documents}</small>{/if}</span></label>
        {:else}<p class="muted">Подключённых наборов пока нет. Добавьте документы в разделе «Данные».</p>{/each}</div>
      {/if}
    </fieldset>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="modal-footer"><button type="button" class="quiet" onclick={close}>Отмена</button><button disabled={saving}>{saving ? 'Сохраняю…' : 'Применить'}</button></div>
  </form>
</Modal>
