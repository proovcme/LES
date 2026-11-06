<script lang="ts">
  import type { Source } from '../lib/contracts';
  import { sourcePlace } from '../lib/contracts';
  import Icon from './Icon.svelte';
  let { source, open }: { source: Source; open: (source: Source) => void } = $props();
</script>
<button class="source-link quiet" onclick={() => open(source)}>
  <Icon name="file" size={18} /><span><strong>{source.doc_name || source.label || `Источник ${source.index}`}</strong><small>Источник {source.index}{sourcePlace(source)}</small></span>
</button>
