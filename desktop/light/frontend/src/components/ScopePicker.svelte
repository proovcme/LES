<script lang="ts">
  import { untrack } from 'svelte';
  import type { Dataset, Scope } from '../lib/contracts';
  import { errorMessage } from '../lib/api';
  import Modal from './Modal.svelte';
  let { scope, datasets, save, close }: { scope: Scope; datasets: Dataset[]; save: (value: Scope) => Promise<void>; close: () => void } = $props();
  let kind = $state(untrack(() => scope.dataset_ids.length ? 'datasets' : scope.scope_type === 'all' ? 'all' : 'none'));
  let selected = $state(untrack(() => [...scope.dataset_ids])); let saving = $state(false); let error = $state('');
  async function submit(event: SubmitEvent) {
    event.preventDefault(); saving = true; error = '';
    try {
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
      <label class="option"><input type="radio" bind:group={kind} value="none" /><span><strong>Без набора документов</strong><small>Обычный диалог или чтение прикреплённого файла.</small></span></label>
      <label class="option"><input type="radio" bind:group={kind} value="all" /><span><strong>Все подключённые документы</strong><small>Лес найдёт подходящие источники сам.</small></span></label>
      <label class="option"><input type="radio" bind:group={kind} value="datasets" /><span><strong>Выбранные наборы</strong><small>Ограничить поиск документами по задаче.</small></span></label>
      {#if kind === 'datasets'}
        <div class="dataset-options">{#each datasets as dataset (dataset.id)}
          <label class="option"><input type="checkbox" bind:group={selected} value={dataset.id} /><span>{dataset.name}<small>Документов: {dataset.document_count ?? 0}</small></span></label>
        {:else}<p class="muted">Подключённых наборов пока нет. Добавьте документы в разделе «Данные».</p>{/each}</div>
      {/if}
    </fieldset>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="modal-footer"><button type="button" class="quiet" onclick={close}>Отмена</button><button disabled={saving}>{saving ? 'Сохраняю…' : 'Применить'}</button></div>
  </form>
</Modal>
