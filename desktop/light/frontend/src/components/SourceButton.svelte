<script lang="ts">
  import type { Source } from '../lib/contracts';
  import { sourcePage } from '../lib/contracts';
  import Icon from './Icon.svelte';
  let { source, open }: { source: Source; open: (source: Source) => void } = $props();
</script>
<button class="source-link quiet" onclick={() => open(source)}>
  <Icon name="file" size={18} /><span><strong>{source.doc_name || source.label || `Источник ${source.index}`}</strong><small>Источник {source.index}{source.source_page || source.page || source.page_number ? ` · стр. ${sourcePage(source)}` : ''}</small></span>
</button>
