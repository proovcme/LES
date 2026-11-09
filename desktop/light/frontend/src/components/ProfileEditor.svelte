<script lang="ts">
  import { onMount } from 'svelte';
  import Modal from './Modal.svelte';
  import { errorMessage, request } from '../lib/api';

  type Revision = { revision_id: string; name: string; revision_no: number; is_factory: boolean };
  type TextRevision = Revision & { text: string };
  type Profile = Revision & { mode: string; prompt_revision_id: string; skill_revision_id: string;
    additional_skills?: Revision[]; tools: string[]; model_policy: Record<string, unknown>; rag_policy: Record<string, unknown> };
  type Group = { mode: string; label: string; active_revision_id: string; active: Profile; revisions: Profile[] };
  type Catalog = { profiles: Group[]; prompt_revisions: TextRevision[]; skill_revisions: TextRevision[];
    tools: { name: string; title?: string }[] };
  let { close, session }: { close: () => void; session?: { session_id: string; role: string } } = $props();
  let catalog = $state<Catalog>();
  let mode = $state('agent');
  let selected = $state('');
  let name = $state('');
  let prompt = $state('');
  let skill = $state('');
  let extras = $state<string[]>([]);
  let tools = $state<string[]>([]);
  let temperature = $state<number>();
  let evidence = $state<number>();
  let candidateCount = $state<number>();
  let perDocument = $state<number>();
  let kind = $state<'prompt' | 'skill'>('prompt');
  let textSource = $state('');
  let textName = $state('');
  let body = $state('');
  let preview = $state('');
  let deleting = $state<'profile' | 'text' | null>(null);
  let notice = $state('');
  let error = $state('');
  let busy = $state(false);
  const group = $derived(catalog?.profiles.find((item) => item.mode === mode));
  const revision = $derived(group?.revisions.find((item) => item.revision_id === selected));
  const texts = $derived(kind === 'prompt' ? catalog?.prompt_revisions : catalog?.skill_revisions);
  const send = <T,>(url: string, method: string, value?: object) =>
    request<T>(url, { method, body: value ? JSON.stringify(value) : undefined });
  const base = '/api/profiles';
  const label = (item: Revision) => `${item.name} · ${item.revision_no}`;
  const numberField = (value: unknown) => typeof value === 'number' ? value : undefined;

  async function run(fn: () => Promise<void>) {
    if (busy) return;
    busy = true; error = ''; notice = '';
    try { await fn(); } catch (e) { error = errorMessage(e); } finally { busy = false; }
  }
  function edit(item?: Profile) {
    selected = item?.revision_id ?? ''; name = item?.name ?? '';
    prompt = item?.prompt_revision_id ?? ''; skill = item?.skill_revision_id ?? '';
    extras = item?.additional_skills?.map((value) => value.revision_id) ?? [];
    tools = [...(item?.tools ?? [])]; preview = ''; deleting = null;
    temperature = numberField(item?.model_policy?.temperature);
    evidence = numberField(item?.rag_policy?.model_evidence_k);
    candidateCount = numberField(item?.rag_policy?.retrieval_candidate_k);
    perDocument = numberField(item?.rag_policy?.document_diversity_k);
  }
  async function load() {
    catalog = await request<Catalog>(base);
  }
  onMount(() => { void run(async () => {
    mode = session?.role === 'search' ? 'search' : 'agent';
    await load(); edit(catalog?.profiles.find((item) => item.mode === mode)?.active);
  }); });

  async function publish() {
    if (!revision || !name.trim() || !prompt || !skill) return;
    const available = new Set(catalog?.tools.map((item) => item.name));
    const item = await send<Profile>(base + '/revisions', 'POST', {
      mode, name: name.trim(), prompt_revision_id: prompt, skill_revision_id: skill,
      additional_skill_revision_ids: extras.filter((id) => id !== skill),
      tools: tools.filter((tool) => available.has(tool)),
      model_policy: { ...revision.model_policy, temperature },
      rag_policy: { ...revision.rag_policy, model_evidence_k: evidence,
        retrieval_candidate_k: candidateCount, document_diversity_k: perDocument },
      source_revision_id: selected,
    });
    await load(); edit(item); notice = 'Новая редакция сохранена.';
  }
  async function activate() {
    await send(`${base}/${mode}/activate/${encodeURIComponent(selected)}`, 'POST');
    await load(); notice = 'Редакция выбрана по умолчанию для новых диалогов этого режима.';
  }
  async function bindChat() {
    if (!session || session.role !== mode) return;
    await send(`${base}/chats/${encodeURIComponent(session.session_id)}/binding`, 'PUT', { mode, profile_revision_id: selected });
    notice = 'Редакция выбрана для текущего диалога.';
  }
  function editText(id = '') {
    const item = texts?.find((value) => value.revision_id === id);
    textSource = id; textName = item?.name ?? ''; body = item?.text ?? ''; deleting = null;
  }
  async function saveText() {
    const item = await send<TextRevision>(base + '/text-revisions', 'POST', {
      kind, name: textName.trim(), text: body, ...(textSource ? { source_revision_id: textSource } : {}),
    });
    await load(); editText(item.revision_id);
    if (kind === 'prompt') prompt = item.revision_id; else skill = item.revision_id;
    notice = 'Текст сохранён. Чтобы применить его, сохраните редакцию профиля.';
  }
  async function removeRevision() {
    const profile = deleting === 'profile';
    await send(`${base}/${profile ? 'profile' : kind}/${encodeURIComponent(profile ? selected : textSource)}`, 'DELETE');
    await load();
    if (profile) edit(catalog?.profiles.find((item) => item.mode === mode)?.active);
    else editText();
    notice = 'Редакция удалена из списка.';
  }
</script>

<Modal title="Профили чата" {close} editor beforeClose={async () => { if (busy) throw new Error(); }}>
  <p role="status">{busy ? 'Выполняю…' : notice}</p>
  {#if error}<p role="alert">{error} <button class="quiet" disabled={busy} onclick={() => run(load)}>Обновить список</button></p>{/if}
  {#if catalog}
    <form class="editor-form" onsubmit={(event) => { event.preventDefault(); void run(publish); }}>
      <label>Режим <select value={mode} disabled={busy} onchange={(event) => {
        mode = event.currentTarget.value; edit(catalog?.profiles.find((item) => item.mode === mode)?.active);
      }}>{#each catalog.profiles as item (item.mode)}<option value={item.mode}>{item.label}</option>{/each}</select></label>
      <label>Редакция <select value={selected} disabled={busy} onchange={(event) => edit(group?.revisions.find((item) => item.revision_id === event.currentTarget.value))}>
        {#each group?.revisions ?? [] as item (item.revision_id)}<option value={item.revision_id}>{label(item)}{item.revision_id === group?.active_revision_id ? ' · по умолчанию' : ''}</option>{/each}
      </select></label>
      <div class="modal-footer">
        <button type="button" class="quiet" disabled={busy || !revision} onclick={() => run(async () => {
          preview = (await request<{ text: string }>(`${base}/revisions/${encodeURIComponent(selected)}/instructions`)).text;
        })}>Посмотреть инструкции</button>
        <button type="button" class="quiet" disabled={busy || !revision || selected === group?.active_revision_id} onclick={() => run(activate)}>Выбрать по умолчанию</button>
        {#if session?.role === mode}<button type="button" class="quiet" disabled={busy || !revision} onclick={() => run(bindChat)}>Применить в этом диалоге</button>{/if}
        {#if revision && !revision.is_factory && selected !== group?.active_revision_id}<button type="button" class="quiet" disabled={busy} onclick={() => deleting = 'profile'}>Удалить редакцию профиля</button>{/if}
      </div>
      {#if preview}<pre>{preview}</pre>{/if}
      <label>Название новой редакции <input required maxlength="160" bind:value={name} disabled={busy} /></label>
      <label>Инструкция <select bind:value={prompt} disabled={busy} required><option value="">Выберите инструкцию</option>
        {#each catalog.prompt_revisions as item (item.revision_id)}<option value={item.revision_id}>{label(item)}</option>{/each}
      </select></label>
      <label>Основной навык <select bind:value={skill} disabled={busy} required><option value="">Выберите навык</option>
        {#each catalog.skill_revisions as item (item.revision_id)}<option value={item.revision_id}>{label(item)}</option>{/each}
      </select></label>
      <details><summary>Дополнительные навыки</summary>
        {#each catalog.skill_revisions.filter((item) => item.revision_id !== skill) as item (item.revision_id)}
          <label class="option"><input type="checkbox" bind:group={extras} value={item.revision_id} disabled={busy} /><span>{label(item)}</span></label>
        {/each}
      </details>
      <details><summary>Инструменты · {tools.filter((id) => catalog?.tools.some((item) => item.name === id)).length}</summary>
        {#if tools.some((id) => !catalog?.tools.some((item) => item.name === id))}<p>Недоступные инструменты не попадут в новую редакцию.</p>{/if}
        {#each catalog.tools as item (item.name)}
          <label class="option"><input type="checkbox" bind:group={tools} value={item.name} disabled={busy} /><span>{item.title || item.name}</span></label>
        {/each}
      </details>
      <details><summary>Параметры ответа и поиска</summary>
        <label class="field">Температура модели <input type="number" min="0" max="2" step="any" bind:value={temperature} disabled={busy} /></label>
        <label class="field">Источников в ответе <input type="number" min="1" max="64" bind:value={evidence} disabled={busy} /></label>
        <label class="field">Кандидатов при поиске <input type="number" min="1" max="512" bind:value={candidateCount} disabled={busy} /></label>
        <label class="field">Фрагментов из одного документа <input type="number" min="1" max="32" bind:value={perDocument} disabled={busy} /></label>
      </details>
      <button disabled={busy || !revision || !prompt || !skill || extras.filter((id) => id !== skill).length > 15}>Сохранить новую редакцию</button>
    </form>
    <details><summary>Редактор инструкций и навыков</summary>
      <form class="editor-form" onsubmit={(event) => { event.preventDefault(); void run(saveText); }}>
        <label>Тип текста <select value={kind} disabled={busy} onchange={(event) => {
          kind = event.currentTarget.value === 'skill' ? 'skill' : 'prompt'; editText();
        }}><option value="prompt">Инструкция</option><option value="skill">Навык</option></select></label>
        <label>Исходный текст <select value={textSource} disabled={busy} onchange={(event) => editText(event.currentTarget.value)}>
          <option value="">Создать текст</option>
          {#each texts ?? [] as item (item.revision_id)}<option value={item.revision_id}>{label(item)}</option>{/each}
        </select></label>
        <label>Название <input required maxlength="160" bind:value={textName} disabled={busy} /></label>
        <label>Текст <textarea required rows="6" maxlength={kind === 'prompt' ? 16000 : 8000} bind:value={body} disabled={busy}></textarea></label>
        <button disabled={busy || !body.trim() || !textName.trim()}>Сохранить редакцию текста</button>
        {#if texts?.some((item) => item.revision_id === textSource && !item.is_factory)}<button type="button" class="quiet" disabled={busy} onclick={() => deleting = 'text'}>Удалить редакцию текста</button>{/if}
      </form>
    </details>
    {#if deleting}
      <p>Удалить редакцию «{deleting === 'profile' ? revision?.name : texts?.find((item) => item.revision_id === textSource)?.name}»?</p>
      <div class="modal-footer">
        <button class="quiet" disabled={busy} onclick={() => deleting = null}>Отмена</button>
        <button disabled={busy} onclick={() => run(removeRevision)}>Подтвердить удаление</button>
      </div>
    {/if}
  {/if}
</Modal>

<style>
  form { display: grid; gap: 12px; margin-block: 12px; }
  form > button { justify-self: start; }
  pre { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 16rem; overflow: auto; }
</style>
