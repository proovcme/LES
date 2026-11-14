<script lang="ts">
  let { reason = '' }: { reason?: string } = $props();
</script>
<aside aria-label="Установка Увязки">
  {#if reason === 'ENGINEERING_UNAVAILABLE'}
    <p>Увязка не найдена по указанному адресу. Если она уже установлена — запустите её и повторите подключение. Если нет — установите с GitHub.</p>
  {:else if reason === 'ENGINEERING_VIEWER_UPDATE_REQUIRED'}
    <p>Увязка отвечает, но вьювер ещё не поддерживает открытие объектов из Леса. Потребуется совместимое обновление.</p>
  {:else}<p class="muted">Увязка устанавливается отдельно. Пока её нет, обычный поиск по документам Леса продолжает работать.</p>{/if}
  <div class="source-actions">
    <a class="button quiet" href="https://github.com/proovcme/uvyazka/releases" target="_blank" rel="noopener noreferrer">{reason === 'ENGINEERING_VIEWER_UPDATE_REQUIRED' ? 'Обновления на GitHub ↗' : 'Установить с GitHub ↗'}</a>
    <a href="https://github.com/proovcme/uvyazka/blob/main/docs/INSTALLATION.md" target="_blank" rel="noopener noreferrer">Инструкция установки</a>
  </div>
  <p class="muted">Откроется страница выпусков с файлами для установки. Лес не скачивает и не запускает установщик автоматически.</p>
</aside>
