<script lang="ts">
  import { onMount } from 'svelte';
  import { request, errorMessage } from '../lib/api';
  import { renderAnswer } from '../lib/markdown';
  import Icon from './Icon.svelte';
  let { docId, datasetId, digest, page, pdf, initialOffset = 0, viewSHA }: {
    docId: string; datasetId?: string; digest?: string; page: number; pdf: boolean;
    initialOffset?: number; viewSHA?: string;
  } = $props();
  type Read = { result: { text?: string; passages?: { text: string }[]; next_offset?: number | null;
    view_sha256?: string; index_revision?: string; ocr?: boolean; coverage?: string; unread_visual_count?: number } };
  let result = $state<Read | null>(null); let busy = $state(false); let error = $state('');
  let cursors = $state([0]); let position = $state(0); let alive = true;
  let initialPin = $state<string | undefined>();
  let attempted = 0;
  let controller: AbortController | undefined;
  const text = $derived(result?.result.text ?? result?.result.passages?.map(p => p.text).join('\n\n') ?? '');
  const html = $derived(renderAnswer(text, []).html);
  const next = $derived(result?.result.next_offset);
  onMount(() => { cursors = [initialOffset]; initialPin = viewSHA; void read(0);
    return () => { alive = false; controller?.abort(); }; });
  async function read(index: number) {
    if (busy) return;
    busy = true; error = ''; attempted = index; controller = new AbortController();
    const timer = setTimeout(() => controller?.abort(), 150000);
    try {
      const previous = result?.result;
      const args: Record<string, unknown> = { doc_id: docId, max_chars: 12000, offset: cursors[index] };
      if (datasetId) args.dataset_id = datasetId;
      if (digest) args.source_sha256 = digest;
      if (pdf) args.page = page;
      if (previous?.index_revision) args.index_revision = previous.index_revision;
      const pinnedView = previous?.view_sha256 ?? initialPin;
      if (pinnedView) args.view_sha256 = pinnedView;
      const value = await request<Read>('/api/rag/sources/call', { method: 'POST', signal: controller.signal,
        body: JSON.stringify({ tool: pdf ? 'read_pdf_source' : 'read_source', args }) });
      if (alive) { result = value; position = index; }
    } catch (failure) { if (alive) error = errorMessage(failure); }
    finally { clearTimeout(timer); if (alive) busy = false; }
  }
  function forward() {
    if (next == null || busy) return;
    cursors = [...cursors.slice(0, position + 1), next]; void read(position + 1);
  }
  function restart() { result = null; initialPin = undefined; cursors = [0]; position = 0; void read(0); }
</script>
<section class="source-reading" aria-label="Текст первоисточника" aria-busy={busy}>
  <h3>{pdf ? `Текст страницы ${page}` : 'Текст документа'}</h3>
  {#if busy}<p role="status">Читаю оригинал{pdf ? ' и при необходимости распознаю скан' : ''}…</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p><div class="source-actions"><button class="quiet" disabled={busy} onclick={() => read(attempted)}>Повторить чтение</button><button class="quiet" disabled={busy} onclick={restart}>Начать заново</button></div>{/if}
  {#if result}
    {#if result.result.ocr}<p class="notice">{pdf ? 'Текст распознан со скана.' : 'В документе распознаны встроенные изображения.'} Возможны ошибки букв, чисел и расположения ячеек — проверьте оригинал.</p>{/if}
    {#if result.result.unread_visual_count}<p class="notice">Объектов без распознанного текста: {result.result.unread_visual_count}. Оригиналы сохранены; их содержимое требует визуальной проверки.</p>{/if}
    <div class="answer-text">{@html html}</div>
    <nav class="source-actions" aria-label="Порции текста">
      {#if cursors[0] > 0}<button class="quiet" disabled={busy} onclick={restart}>К началу документа</button>{/if}
      <button class="quiet" disabled={busy || position === 0} onclick={() => read(position - 1)}><Icon name="back" />Предыдущая порция</button>
      <span class="muted">Порция {position + 1}</span>
      <button class="quiet" disabled={busy || next == null} onclick={forward}>Читать дальше</button>
    </nav>
    {#if !error && !busy && next == null}<p class="muted">{cursors[0] > 0 ? 'Достигнут конец извлечённого текста.' : pdf ? 'Все порции текста страницы показаны.' : 'Все порции извлечённого текста показаны.'}</p>{/if}
  {/if}
</section>
