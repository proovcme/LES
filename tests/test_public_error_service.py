from proxy.services.public_error_service import public_error_payload


def test_search_failure_is_not_reported_as_model_outage():
    payload = public_error_payload(status_code=503, detail={
        "error": "MODEL_RAG_SEARCH_INCOMPLETE", "blocked_queries": [1],
        "internal": "password=do-not-leak",
    })
    assert payload["code"] == "MODEL_RAG_SEARCH_INCOMPLETE"
    assert "Поиск по документам" in payload["detail"]
    assert "password" not in str(payload)


def test_empty_query_result_has_its_own_recovery_message():
    payload = public_error_payload(status_code=503, detail="MODEL_RAG_QUERY_LIST_EMPTY")
    assert payload["code"] == "MODEL_RAG_QUERY_LIST_EMPTY"
    assert "поисковые запросы" in payload["detail"]


def test_internal_error_payload_hides_exception_details():
    payload = public_error_payload(
        status_code=500,
        detail="ValueError: password=do-not-leak",
    )

    assert payload == {
        "code": "INTERNAL_CHAT_ERROR",
        "detail": "Не удалось завершить запрос. Повторите попытку или откройте диагностику.",
    }
    assert "password" not in str(payload)
    assert "ValueError" not in str(payload)


def test_model_queue_timeout_keeps_stable_code_and_message():
    payload = public_error_payload(
        status_code=429,
        detail={
            "code": "MODEL_QUEUE_TIMEOUT",
            "detail": "Модель занята. Запрос дождался своей очереди, но время ожидания истекло.",
        },
    )

    assert payload["code"] == "MODEL_QUEUE_TIMEOUT"
    assert "очеред" in payload["detail"]


def test_known_user_rejection_remains_readable():
    payload = public_error_payload(status_code=409, detail="Запрос уже выполняется")

    assert payload == {"code": "REQUEST_REJECTED", "detail": "Запрос уже выполняется"}
