<script lang="ts">
  let { reason = '' }: { reason?: string } = $props();
</script>
{#if reason === 'ENGINEERING_DESKTOP_NOT_INSTALLED' || reason === 'ENGINEERING_VIEWER_UPDATE_REQUIRED'}
<aside aria-label="Установка Увязки">
  <div class="source-actions">
    <a class="button quiet" href="https://github.com/proovcme/uvyazka/releases" target="_blank" rel="noopener noreferrer">{reason === 'ENGINEERING_VIEWER_UPDATE_REQUIRED' ? 'Обновления на GitHub ↗' : 'Установить с GitHub ↗'}</a>
    <a href="https://github.com/proovcme/uvyazka/blob/main/docs/INSTALLATION.md" target="_blank" rel="noopener noreferrer">Инструкция установки</a>
  </div>
</aside>
{/if}
