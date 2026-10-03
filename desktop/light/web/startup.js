const invoke = window.__TAURI__.core.invoke;
const message = document.getElementById('message');
const retry = document.getElementById('retry');
const browser = document.getElementById('browser');
async function refresh() {
  try {
    const status = await invoke('startup_status');
    message.textContent = status.message || 'Подготавливаю приложение…';
    document.body.classList.toggle('error', status.phase === 'error');
    document.getElementById('title').textContent = status.phase === 'error' ? 'Нужна помощь с запуском' : status.phase === 'recovering' ? 'Восстанавливаем подключение' : 'Открываем рабочее пространство';
    retry.hidden = status.phase !== 'error';
    browser.hidden = status.phase !== 'ready';
  } catch (_) { message.textContent = 'Не удалось получить состояние приложения. Повторите запуск.'; retry.hidden = false; }
}
retry.addEventListener('click', async () => { retry.disabled = true; try { await invoke('retry_start'); } catch (error) { message.textContent = String(error); } finally { retry.disabled = false; } });
browser.addEventListener('click', async () => { try { await invoke('open_browser'); } catch (error) { message.textContent = String(error); } });
refresh(); setInterval(refresh, 1000);
