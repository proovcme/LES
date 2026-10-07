# Модули LES RAG

- backend: чтение документов, OCR, индекс Qdrant и политика ресурсов.
- proxy: API, чат, доказательства, память и подключения.
- sovushka: интерфейс NiceGUI.
- desktop/light: оболочка Windows.
- tools/light_launcher.py: собственные процессы приложения.
- installers/windows/light: установка и откат.

Восстановление диалога: `proxy/services/chat_durability_service.py`.
Фоновая сводка: `proxy/services/background_summary_service.py`.
Навыки и вычисления: `installed_skill_service.py`, `skill_compute_service.py`
в `proxy/services`, worker `tools/skill_compute_worker.py`.
Компактный каталог выбранных расширений: `proxy/services/extension_tool_service.py`.
Наблюдение за службами: `backend/light_health_monitor.py`.

[Карта кода](CODE_MAP.md) · [Архитектура](LIGHT_ARCHITECTURE.md)
