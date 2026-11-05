<script lang="ts">
  let { name, size = 20 }: { name: string; size?: number } = $props();
  const paths: Record<string, string> = {
    chat: 'M4 4h16v12H8l-4 4V4Z', plus: 'M12 5v14M5 12h14',
    history: 'M3 11a9 9 0 1 1 2 6M3 4v7h7M12 7v5l3 2',
    tree: 'm12 3-6 8h3l-5 7h7v3h2v-3h7l-5-7h3l-6-8Z',
    folder: 'M3 7V5h6l2 2h10v13H3V7Z', settings: 'M4 7h16M4 17h16M8 4v6M16 14v6',
    arrow: 'M12 19V5M6 11l6-6 6 6', back: 'm10 5-7 7 7 7M3 12h18',
    close: 'm6 6 12 12M6 18 18 6', paperclip: 'm8 12 7-7a4 4 0 0 1 6 6L10 22a6 6 0 0 1-8-8L13 3M5 17 16 6',
    stop: 'M6 6h12v12H6Z', file: 'M5 3h9l5 5v13H5V3Zm9 0v6h5M8 13h8M8 17h6',
    external: 'M14 3h7v7M21 3l-11 11M10 5H3v16h16v-7',
    menu: 'M4 6h16M4 12h16M4 18h16', check: 'm5 12 4 4L19 6',
  };
</script>
<svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={paths[name] || paths.file} /></svg>
