"""Stable operator-facing chat errors without internal exception leakage."""

from __future__ import annotations

from typing import Any


_KNOWN_ERRORS = {
    "INDEX_SPARSE_CONTRACT_INVALID": "Не удалось подтвердить настройки точного поиска. Индекс сохранён; откройте диагностику и восстановите его контракт.",
    "INDEX_SPARSE_STALE": "Точный поиск ещё не обновлён после изменения документов. Повторите поиск: Лес пересчитает веса без обращения к модели.",
    "SECTION_READ_UNAVAILABLE": "Не удалось дочитать раздел документа из индекса. Проверьте состояние набора в «Данных» и повторите вопрос.",
    "SECTION_READ_LIMIT": "Раздел слишком велик для безопасного дочитывания за один запрос. Уточните документ, страницу или часть раздела.",
    "SECTION_SCOPE_MISMATCH": "Не удалось подтвердить принадлежность фрагментов выбранным источникам. Обновите индекс набора и повторите вопрос.",
    "INDEX_RECOVERY_REQUIRED": "Обновление индекса не завершено. Откройте «Данные» и повторите индексацию; Лес восстановит прерванную запись перед продолжением.",
    "INDEX_RECOVERY_INCOMPLETE": "Не удалось восстановить индекс после прерывания: часть фрагментов отсутствует. Откройте диагностику набора; поиск остановлен, чтобы не выдать неполный результат.",
    "INDEX_UPDATE_BUSY": "Лес обновляет индекс документов. Дождитесь завершения в разделе «Данные» и повторите вопрос.",
    "INDEX_CHANGED_DURING_SEARCH": "Документы обновились во время поиска. Повторите вопрос, чтобы получить ответ по одной версии источников.",
    "embedding_contract_mismatch": "Назначенная модель поиска несовместима с индексом документов. Верните прежнюю модель в «Подключениях» или создайте индекс для новой модели. Старый индекс сохранён.",
    "ROLE_BINDING_MISSING": "Модель не выбрана. Откройте «Подключения» и назначьте модель для ответов или поиска.",
    "CONNECTION_DISABLED": "Подключение выбранной модели отключено. Включите его в разделе «Подключения».",
    "CONNECTION_SECRET_MISSING": "Для выбранной модели не найден ключ API. Укажите ключ в настройках подключения.",
    "CAPABILITY_SNAPSHOT_MISSING": "Подключение модели ещё не проверено. Откройте «Подключения» и запустите проверку.",
    "CAPABILITY_SNAPSHOT_STALE": "Проверка подключения модели устарела. Откройте «Подключения» и проверьте модель ещё раз.",
    "CAPABILITY_REQUIRED": "Выбранная модель не поддерживает нужную функцию. Проверьте её возможности в «Подключениях» и выберите подходящую модель.",
    "REASONING_MODE_UNSUPPORTED": "Для облачного подключения протокол размышления не подтверждён. Используйте совместимый локальный движок или выключите размышление в версии профиля.",
    "REASONING_BUDGET_EXHAUSTED": "Модель исчерпала бюджет размышления без итогового ответа. Увеличьте бюджет в версии профиля или выключите размышление.",
    "UPSTREAM_TIMEOUT": "Модель не ответила вовремя. Проверьте, запущена ли она, и повторите вопрос.",
    "UPSTREAM_UNREACHABLE": "Не удаётся связаться с моделью. Запустите её сервис и проверьте адрес в «Подключениях».",
    "UPSTREAM_AUTH_FAILED": "Сервис модели отклонил доступ. Проверьте ключ API и права в настройках подключения.",
    "UPSTREAM_MODEL_NOT_FOUND": "Сервис не нашёл выбранную модель или адрес API. Обновите список моделей и проверьте адрес в «Подключениях».",
    "UPSTREAM_RATE_LIMITED": "Сервис модели временно ограничил запросы. Подождите и повторите вопрос; для облачного API проверьте доступный лимит.",
    "UPSTREAM_STREAM_INTERRUPTED": "Связь с моделью прервалась. Ответ мог остаться незавершённым. Проверьте подключение и повторите вопрос.",
    "UPSTREAM_RESPONSE_INVALID": "Модель вернула ответ, который Лес не смог прочитать. Проверьте совместимость API в «Подключениях» и повторите вопрос.",
    "UPSTREAM_RESPONSE_TOO_LARGE": "Ответ сервиса превысил допустимый размер. Попробуйте более узкий вопрос; подробности сохранены в диагностике.",
    "RERANK_TIMEOUT": "Реранкер не ответил вовремя. Отключите «Уточнять порядок источников» и повторите вопрос.",
    "MODEL_RAG_QUERY_LIST_EMPTY": "Модель не вернула поисковые запросы. Повторите запрос или проверьте выбранную модель в настройках подключений.",
    "MODEL_RAG_SEARCH_INCOMPLETE": "Поиск по документам не завершён. Проверьте готовность набора и подключение к индексу, затем повторите запрос.",
}


_SERVER_ERRORS: dict[int, tuple[str, str]] = {
    500: (
        "INTERNAL_CHAT_ERROR",
        "Не удалось завершить запрос. Повторите попытку или откройте диагностику.",
    ),
    502: (
        "MODEL_UPSTREAM_ERROR",
        "Назначенная модель не смогла завершить запрос. Повторите попытку.",
    ),
    503: (
        "MODEL_SERVICE_UNAVAILABLE",
        "Сервис модели временно недоступен. Проверьте подключение или повторите запрос.",
    ),
    504: (
        "MODEL_TIMEOUT",
        "Истекло время ожидания ответа модели. Повторите запрос.",
    ),
}


def public_error_payload(*, status_code: int, detail: Any) -> dict[str, str]:
    """Return the only error fields allowed across the chat/UI boundary."""

    known_code = (
        str(detail.get("code") or detail.get("error") or "")
        if isinstance(detail, dict) else str(detail or "")
    )
    prefix, _, suffix = known_code.partition(":")
    if prefix == "UPSTREAM_HTTP_ERROR":
        known_code = {"401": "UPSTREAM_AUTH_FAILED", "403": "UPSTREAM_AUTH_FAILED",
                      "404": "UPSTREAM_MODEL_NOT_FOUND", "429": "UPSTREAM_RATE_LIMITED"}.get(suffix.strip(), prefix)
    elif prefix == "UPSTREAM_REQUEST_FAILED":
        known_code = "UPSTREAM_TIMEOUT" if "Timeout" in suffix else "UPSTREAM_UNREACHABLE"
    elif prefix in _KNOWN_ERRORS:
        known_code = prefix
    if known_code in _KNOWN_ERRORS:
        return {"code": known_code, "detail": _KNOWN_ERRORS[known_code]}

    if isinstance(detail, dict):
        code = str(detail.get("code") or "").strip()
        message = str(detail.get("detail") or detail.get("message") or "").strip()
        if code and message:
            return {"code": code, "detail": message}

    if status_code >= 500:
        code, message = _SERVER_ERRORS.get(status_code, _SERVER_ERRORS[500])
        return {"code": code, "detail": message}

    message = str(detail or "Запрос отклонён").strip()
    return {"code": "REQUEST_REJECTED", "detail": message}
