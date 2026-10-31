<script lang="ts">
  import { onMount } from 'svelte';
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  let { title, close, children, wide = false }: { title: string; close: () => void; children: Snippet; wide?: boolean } = $props();
  let dialog: HTMLDialogElement;
  onMount(() => { dialog.showModal(); return () => dialog.close(); });
</script>
<dialog bind:this={dialog} class:wide onclose={close} aria-label={title}>
  <div class="modal-heading"><h2>{title}</h2><button class="icon-button quiet" aria-label="Закрыть и вернуться" onclick={() => dialog.close()}><Icon name="close" /></button></div>
  <div class="modal-body">{@render children()}</div>
</dialog>
