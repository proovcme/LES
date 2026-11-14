<script lang="ts">
  import { onMount } from 'svelte';
  import { request, errorMessage, ApiError } from '../lib/api';
  let { docId, digest }: { docId: string; digest?: string } = $props();
  type Part = { sha256: string; label: string; kind: string; member: string; url: string; preview_url?: string;
    coverage?: string; recognition_method?: string; views?: { sha256: string; url: string }[] };
  type Page = { parts: Part[]; next_offset: number | null; total: number };
  type Detail = { text: string; next_offset?: number | null; visual_verification_required: boolean };
  let page = $state<Page | null>(null); let selected = $state<Part | null>(null); let detail = $state<Detail | null>(null);
  let busy = $state(false); let error = $state(''); let imageError = $state(''); let imageLoading = $state(false);
  let busyLabel = $state('');
  let imageURL = $state(''); let originalURL = $state(''); let view = $state(0); let offsets = $state([0]); let position = $state(0); let alive = true;
  let attempted = $state<(() => Promise<void>) | null>(null); let controller: AbortController | undefined;
  function localURL(value?: string): string {
    if (!value) return '';
    const url = new URL(value, location.href);
    if (url.origin !== location.origin || !url.pathname.startsWith(`/api/documents/by-id/${encodeURIComponent(docId)}/parts/`) ||
      (digest && url.searchParams.get('source_sha256') !== digest)) throw new ApiError('Ссылка рисунка не соответствует оригиналу.');
    return url.pathname + url.search;
  }
  async function invoke(args: Record<string, unknown>) {
    controller = new AbortController();
    const timeout = setTimeout(() => controller?.abort(), 30000);
    try { return await request<{ result: Page & Detail }>('/api/rag/sources/call', { method: 'POST', signal: controller.signal,
      body: JSON.stringify({ tool: 'read_source', args: { doc_id: docId, ...(digest ? { source_sha256: digest } : {}), ...args } }) }); }
    finally { clearTimeout(timeout); }
  }
  async function load(index: number) {
    if (busy) return;
    busy = true; busyLabel = 'Ищу рисунки и вложения…'; error = ''; attempted = () => load(index);
    try {
      let cursor = offsets[index];
      // Structure records remain available to the shared reader, but raw XML
      // is not a user-facing picture. Skip bounded empty pages automatically.
      for (let scan = 0; scan < 16; scan++) {
        const reply = await invoke({ view: 'parts', offset: cursor, limit: 16 });
        if (!alive) return;
        const visible = reply.result.parts.filter(part => !['structure', 'source'].includes(part.kind));
        const next = reply.result.next_offset;
        if (next != null && next <= cursor) throw new ApiError('Лес не смог продолжить список. Откройте рисунки заново.');
        if (visible.length || next == null || scan === 15) {
          page = { ...reply.result, parts: visible }; position = index; selected = null; detail = null; imageURL = ''; break;
        }
        cursor = next;
      }
    }
    catch (failure) { if (alive) error = errorMessage(failure); }
    finally { if (alive) busy = false; }
  }
  async function open(part: Part, index = 0, offset = 0) {
    if (busy) return;
    busy = true; busyLabel = part.kind === 'image' ? 'Открываю рисунок…' : 'Читаю вложение…'; error = ''; attempted = () => open(part, index, offset);
    const changed = selected?.sha256 !== part.sha256 || view !== index;
    if (changed) { imageURL = ''; detail = null; imageError = ''; }
    try {
      const variant = part.views?.[index];
      const preview = part.kind === 'image' ? localURL(variant?.url ?? part.preview_url) : '';
      const original = localURL(part.url);
      const reply = await invoke({ view: 'part', part_id: variant?.sha256 ?? part.sha256, member: part.member, offset, max_chars: 4000 });
      if (alive) { selected = part; view = index; detail = reply.result; originalURL = original;
        if (changed || imageError) { imageURL = preview; imageLoading = Boolean(preview); imageError = ''; } }
    } catch (failure) { if (alive) error = errorMessage(failure); }
    finally { if (alive) busy = false; }
  }
  function forward() { if (page?.next_offset != null) { offsets = [...offsets.slice(0, position + 1), page.next_offset]; void load(position + 1); } }
  onMount(() => { void load(0); return () => { alive = false; controller?.abort(); }; });
</script>
<section aria-label="Рисунки и вложения" aria-busy={busy}>
  <h3>Рисунки и вложения</h3>
  {#if busy}<p role="status">{busyLabel}</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p><button class="quiet" disabled={busy} onclick={() => attempted?.()}>Повторить</button>{/if}
  {#if page}
    {#if page.parts.length === 0}<p class="muted">В этой порции нет рисунков или вложений.</p>{/if}
    <ul class="session-list">
      {#each page.parts as part}<li><button class="quiet source-link" disabled={busy} onclick={() => open(part)}>{part.label}</button></li>{/each}
    </ul>
    <nav class="source-actions" aria-label="Порции объектов">
      <button class="quiet" disabled={busy || position === 0} onclick={() => load(position - 1)}>Назад</button>
      <span class="muted">Порция {position + 1}</span>
      <button class="quiet" disabled={busy || page.next_offset == null} onclick={forward}>Ещё объекты</button>
    </nav>
  {/if}
  {#if selected && detail}
    <h4>{selected.label}</h4>
    {#if selected.kind === 'image'}<p class="notice">Подписи и OCR не объясняют геометрию и связи на схеме. Проверьте само изображение перед выводом.</p>{/if}
    {#if (selected.views?.length ?? 0) > 1}<nav class="source-actions" aria-label="Виды рисунка">
      <button class="quiet" disabled={busy || view === 0} onclick={() => open(selected!, view - 1)}>Предыдущий вид</button>
      <span>Вид {view + 1} из {selected.views?.length}</span>
      <button class="quiet" disabled={busy || view + 1 >= (selected.views?.length ?? 0)} onclick={() => open(selected!, view + 1)}>Следующий вид</button>
    </nav>{/if}
    {#if imageURL}
      {#if imageLoading}<p role="status">Показываю изображение…</p>{/if}
      <img class="pdf-page" style:display={imageLoading ? 'none' : undefined} src={imageURL} alt={selected.label}
        onload={() => imageLoading = false} onerror={() => { imageLoading = false; imageURL = ''; imageError = 'Изображение не открылось. Повторите чтение объекта.'; }} />
    {/if}
    {#if imageError}<p class="error" role="alert">{imageError}</p><button class="quiet" disabled={busy} onclick={() => open(selected!, view)}>Повторить изображение</button>{/if}
    {#if detail.text}<p class="evidence">{detail.text}</p>{:else if selected.kind === 'image'}<p class="muted">Надписи не прочитаны. Это не означает, что изображение пустое.</p>{:else}<p class="muted">Вложение сохранено. Откройте исходный объект для просмотра.</p>{/if}
    {#if detail.next_offset != null}<button class="quiet" disabled={busy} onclick={() => open(selected!, view, detail!.next_offset!)}>Читать подписи дальше</button>{/if}
    <details><summary>Исходный объект</summary><p>{selected.member}</p><a class="button quiet" href={originalURL}>Открыть исходный объект</a></details>
  {/if}
</section>
