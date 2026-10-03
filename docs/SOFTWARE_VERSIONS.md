# Версии LES RAG

Источник версии — [config/version.json](../config/version.json).

| Поле | Значение |
|---|---|
| Версия продукта | `0.1.0` |
| Номер сборки | `744` |
| Версия пакета Tauri/NSIS | `0.1.0` |

Windows x64, WebView2 Evergreen. Python 3.13.12 и собственный Qdrant 1.19.1
входят в пакет. Архивы закреплены SHA-256 в config/. Docker не требуется.
Ollama и совместимый OpenAI API подключаются пользователем; веса моделей
в пакет не входят. Роли чата и поиска назначаются явно.

Python-зависимости закреплены в uv.lock; сборка использует uv export --frozen
и проверку хешей. PDF: pdfplumber и PDFium; Outlook MSG: python-oxmsg.
Сканы: Windows OCR с установленными языками. Rust: desktop/light/src-tauri/Cargo.lock.

Изменение зависимости требует обновления lock, make verify, make test,
make test-mail и повторной приёмки установленного приложения.
Лицензии поставляемых компонентов входят в THIRD_PARTY_NOTICES.
