<script lang="ts">
  import { onMount } from 'svelte';
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  let { title, close, children, wide = false, beforeClose }: { title: string; close: () => void; children: Snippet; wide?: boolean; beforeClose?: () => Promise<void> } = $props();
  let dialog: HTMLDialogElement;
  onMount(() => { dialog.showModal(); return () => dialog.close(); });
  async function requestClose() { try { await beforeClose?.(); dialog.close(); } catch { /* The editor displays the save error and retains focus/data. */ } }
</script>
<dialog bind:this={dialog} class:wide onclose={close} oncancel={(event) => { event.preventDefault(); void requestClose(); }} aria-label={title}>
  <div class="modal-heading"><h2>{title}</h2><button class="icon-button quiet" aria-label="Закрыть и вернуться" onclick={requestClose}><Icon name="close" /></button></div>
  <div class="modal-body">{@render children()}</div>
</dialog>
