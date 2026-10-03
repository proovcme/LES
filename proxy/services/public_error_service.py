"""Stable operator-facing chat errors without internal exception leakage."""

from __future__ import annotations

from typing import Any


_KNOWN_ERRORS = {
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
