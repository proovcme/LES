<script lang="ts">
  import { onMount } from 'svelte';
  import { ApiError, errorMessage, request } from '../lib/api';
  import type { Source } from '../lib/contracts';
  import { sourcePage, sourcePlace } from '../lib/contracts';
  import Modal from './Modal.svelte';
  import Icon from './Icon.svelte';
  import SourceReader from './SourceReader.svelte';
  import SourceFigures from './SourceFigures.svelte';
  import EngineeringSource from './EngineeringSource.svelte';
  let { source, close }: { source: Source; close: () => void } = $props();
  let loading = $state(true); let error = $state(''); let documentError = $state('');
  let doc = $state<{ id: string; file_name: string } | null>(null);
  let preview = $state(false); let preparing = $state(false);
  let reading = $state(false);
  let figures = $state(false);
  let page = $state(1); let imageURL = $state(''); let viewURL = $state('');
  let imageLoading = $state(false);
  let alive = true;
  const identity = $derived(new URLSearchParams({
    dataset_id: source.dataset_id || '', doc_name: source.doc_name || '',
    source_sha256: source.locator?.source_sha256 || '',
  }));
  const base = $derived(doc ? `/api/documents/by-id/${encodeURIComponent(doc.id)}` : '');
  onMount(() => {
    page = sourcePage(source);
    void request<{ document: { id: string; file_name: string } }>(`/api/documents/by-id/${encodeURIComponent(source.doc_id || 'source')}?${identity}`)
      .then((value) => { if (alive) doc = value.document; })
      .catch((failure) => { if (alive) error = errorMessage(failure); })
      .finally(() => { if (alive) loading = false; });
    return () => { alive = false; if (imageURL) URL.revokeObjectURL(imageURL); };
  });
  async function showOriginal() {
    if (!doc || preparing) return;
    preparing = true; documentError = '';
    try {
      if (doc.file_name.toLowerCase().endsWith('.pdf')) {
        const result = await fetch(`${base}/pdf-contour/pages/${page}/preview?${identity}`, { signal: AbortSignal.timeout(30000) });
        if (!result.ok) {
          const body = await result.json().catch(() => ({}));
          throw new ApiError(typeof body.detail === 'string' ? body.detail : 'Не удалось открыть страницу.');
        }
        const blob = await result.blob();
        if (!alive) return;
        if (imageURL) URL.revokeObjectURL(imageURL);
        imageURL = URL.createObjectURL(blob); viewURL = ''; imageLoading = true;
      } else {
        const params = new URLSearchParams(identity);
        params.set('page', String(page));
        const { sheet, row, anchor } = source.locator ?? {};
        if (sheet && row) {
          params.set('sheet', sheet);
          params.set('locator', `${sheet}!R${row}`);
        } else if (anchor) {
          params.set('locator', anchor);
        }
        const url = `${base}/viewer?${params}`;
        const response = await fetch(url, { signal: AbortSignal.timeout(30000) });
        if (!response.ok) {
          const body = await response.json().catch(() => ({}));
          throw new ApiError(typeof body.detail === 'string' ? body.detail : 'Не удалось открыть документ.');
        }
        await response.body?.cancel();
        if (!alive) return;
        viewURL = url;
      }
      preview = true;
    } catch (failure) { documentError = errorMessage(failure); }
    finally { preparing = false; }
  }
  async function openNative() {
    documentError = '';
    try { await request(`${base}/open-native?${identity}`, { method: 'POST' }); }
    catch (failure) { documentError = errorMessage(failure); }
  }
</script>
<Modal title={source.model_locator?.source_name || source.doc_name || 'Доказательство'} {close} wide>
  <p class="eyebrow">Источник {source.index}{sourcePlace(source)}</p>
  {#if source.locator?.section_heading || source.section_heading}<p>{source.locator?.section_heading || source.section_heading}</p>{/if}
  {#if source.quote || source.snippet}<blockquote class="evidence">{source.quote || source.snippet}</blockquote>{:else}<p class="muted">У этого источника не сохранена цитата. Проверьте оригинал.</p>{/if}
  {#if loading}<p role="status">Проверяю оригинал…</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if source.model_locator}<EngineeringSource target={source.model_locator} />{/if}
  {#if doc}
    {#if !source.locator?.source_sha256}<p class="muted">Для этой цитаты редакция файла не сохранена. Доступен текущий оригинал.</p>{/if}
    <div class="source-actions">
      <button class="quiet" onclick={() => reading = !reading}>{reading ? 'Скрыть текст' : 'Прочитать текст'}</button>
      {#if /\.docx$/i.test(doc.file_name)}<button class="quiet" onclick={() => figures = !figures}>{figures ? 'Скрыть рисунки' : 'Рисунки и вложения'}</button>{/if}
      <button onclick={showOriginal} disabled={preparing}><Icon name="file" />{preparing ? 'Открываю…' : source.model_locator ? 'Показать снимок данных' : 'Показать оригинал'}</button>
      <a class="button quiet" href={`${base}/raw?${identity}`} download={doc.file_name}><Icon name="external" />{source.model_locator ? 'Скачать снимок' : 'Скачать'}</a>
      <button class="quiet" onclick={openNative}>{source.model_locator ? 'Открыть файл снимка' : 'Открыть в приложении'}</button>
    </div>
    {#if reading}
      {#key `${doc.id}:${page}`}
        <SourceReader docId={doc.id} datasetId={source.dataset_id} digest={source.locator?.source_sha256}
          initialOffset={source.evidence_kind === 'physical_word_section' && typeof source.ordinal === 'number' && Number.isInteger(source.ordinal) && source.ordinal >= 0 && /^[a-f0-9]{64}$/i.test(source.view_sha256 ?? '') ? source.ordinal : 0}
          viewSHA={source.view_sha256}
          {page} pdf={doc.file_name.toLowerCase().endsWith('.pdf')} />
      {/key}
    {/if}
    {#if figures}<SourceFigures docId={doc.id} digest={source.locator?.source_sha256} />{/if}
    {#if preview && doc.file_name.toLowerCase().endsWith('.pdf')}
      <form class="page-selector" onsubmit={(event) => { event.preventDefault(); void showOriginal(); }}>
        <label for="source-page">Страница</label><input id="source-page" type="number" min="1" step="1" bind:value={page} required /><button class="quiet" disabled={preparing}>Перейти</button>
      </form>
    {/if}
    {#if documentError}<p class="error" role="alert">{documentError}</p>{/if}
    {#if imageURL && preview}
      {#if imageLoading}<p role="status">Показываю страницу…</p>{/if}
      <img class="pdf-page" src={imageURL} alt={`Страница ${page}: ${doc.file_name}`} onload={() => imageLoading = false} onerror={() => { imageLoading = false; documentError = 'Не удалось отобразить страницу.'; }} />
    {:else if viewURL && preview}
      <iframe class="document-frame" title={`Оригинал: ${doc.file_name}`} src={viewURL} sandbox="allow-scripts allow-downloads"></iframe>
    {/if}
  {/if}
  <div class="modal-footer"><button class="quiet" onclick={close}><Icon name="back" />Вернуться к ответу</button></div>
</Modal>
