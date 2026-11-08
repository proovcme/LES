use axum::http::StatusCode;
use serde_json::{json, Value};
#[derive(Debug)]
pub struct Failure {
    pub code: String,
    pub status: StatusCode,
}
impl From<&str> for Failure {
    fn from(code: &str) -> Self {
        Self::new(code)
    }
}
impl From<String> for Failure {
    fn from(code: String) -> Self {
        Self::new(&code)
    }
}
impl Failure {
    pub fn new(code: &str) -> Self {
        let upstream = code.strip_prefix("UPSTREAM_HTTP_ERROR:").map(str::trim);
        let code = match upstream {
            Some("401") => "UPSTREAM_AUTHENTICATION_FAILED",
            Some("403") => "UPSTREAM_ACCESS_DENIED",
            Some("404") => "UPSTREAM_MODEL_NOT_FOUND",
            Some("429") => "UPSTREAM_RATE_LIMITED",
            Some("502" | "503" | "504") => "UPSTREAM_SERVICE_UNAVAILABLE",
            _ => code,
        };
        let key = code.split(':').next().unwrap_or(code);
        let code = if !key.is_empty()
            && key.len() <= 100
            && key
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        {
            key
        } else {
            "CHAT_EXECUTION_FAILED"
        };
        let status = if matches!(
            code,
            "CHAT_CANCELLED" | "CHAT_REQUEST_NOT_REPLAYABLE" | "IDEMPOTENCY_KEY_REUSED"
        ) {
            StatusCode::CONFLICT
        } else if code.starts_with("CHAT_REQUEST")
            || code.starts_with("CHAT_TEXT")
            || code == "CHAT_QUESTION_REQUIRED"
            || code == "CHAT_RESPONSE_LENGTH_INVALID"
        {
            StatusCode::UNPROCESSABLE_ENTITY
        } else if matches!(code, "MODEL_QUEUE_TIMEOUT" | "CHAT_SERVICE_BUSY") {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        };
        Self {
            code: code.into(),
            status,
        }
    }
    pub fn payload(&self) -> Value {
        let detail=match self.code.as_str(){
            "CHAT_QUESTION_REQUIRED"=>"Введите сообщение.",
            "CHAT_TEXT_LIMIT"=>"Текст слишком большой. Прикрепите документ файлом или сократите сообщение.",
            "CHAT_SCOPE_EMPTY"=>"В выбранной области нет доступных документов. Добавьте документы или выберите другой набор.",
            "CHAT_SCOPE_INVALID"=>"Не удалось определить выбранные документы. Выберите область поиска ещё раз.",
            "MODEL_QUEUE_TIMEOUT"|"CHAT_SERVICE_BUSY"=>"Модель занята. Дождитесь завершения текущего запроса и повторите.",
            "CHAT_MEMORY_GUARD"=>"Недостаточно свободной оперативной памяти для модели. Освободите не менее 4 ГБ и повторите запрос.",
            "CONNECTION_ROLE_UNASSIGNED"|"ANSWER_MODEL_NOT_ASSIGNED"=>"Модель для чата не назначена. Подключите модель и выберите её для ответов в настройках.",
            "CONNECTION_DISABLED"=>"Подключение модели отключено. Включите его в настройках.",
            "CONNECTION_SECRET_MISSING"=>"Не найден ключ API модели. Проверьте подключение в настройках.",
            "CLOUD_CONSENT_REQUIRED"=>"Разрешите использование подключённой внешней модели в настройках.",
            "CAPABILITY_SNAPSHOT_STALE"|"CAPABILITY_SNAPSHOT_MISSING"|"CAPABILITY_SNAPSHOT_EXPIRED"=>"Обновите проверку подключения модели в настройках.",
            "UPSTREAM_TIMEOUT"|"MODEL_TIMEOUT"=>"Модель не ответила вовремя. Проверьте её состояние и повторите запрос.",
            "UPSTREAM_AUTHENTICATION_FAILED"=>"Сервис модели отклонил ключ API. Проверьте ключ в настройках подключения.",
            "UPSTREAM_ACCESS_DENIED"=>"Сервис запретил доступ к модели. Проверьте права подключения.",
            "UPSTREAM_MODEL_NOT_FOUND"=>"Сервис не нашёл назначенную модель. Проверьте её имя и адрес подключения.",
            "UPSTREAM_RATE_LIMITED"=>"Сервис модели ограничил число запросов. Подождите немного и повторите.",
            "UPSTREAM_SERVICE_UNAVAILABLE"=>"Сервис модели сейчас недоступен. Проверьте, запущен ли он, и повторите запрос.",
            "UPSTREAM_UNREACHABLE"|"UPSTREAM_STREAM_INTERRUPTED"=>"Связь с моделью прервалась. Сохранённая часть ответа доступна в истории; проверьте сервер модели.",
            "CONTEXT_REQUIRED_SECTION_OVERFLOW"=>"Выбранные документы и обязательные результаты не помещаются в контекст модели. Сузьте задачу или выберите модель с большим контекстом.",
            "INDEX_CHANGED_DURING_SEARCH"|"PDF_REVISION_CHANGED"=>"Документ изменился во время чтения. Повторите запрос для текущей редакции.",
            "CHAT_CANCELLED"=>"Ответ остановлен. Уже выполненные действия и прочитанная часть сохранены.",
            "CHAT_REQUEST_NOT_REPLAYABLE"=>"Этот запрос уже выполняется или был прерван. Автоматический повтор отключён, чтобы не повторить действия с файлами. Проверьте историю и отправьте новый запрос.",
            "IDEMPOTENCY_KEY_REUSED"=>"Ключ повторного запроса уже использован для другого сообщения. Отправьте сообщение с новым ключом.",
            "CHAT_DEADLINE_EXCEEDED"=>"Истекло время выполнения запроса. Полученный текст сохранён; сузьте задачу или повторите запрос.",
            "MODEL_OUTPUT_LIMIT_REACHED"=>"Ответ достиг лимита длины и не завершён. Полученный текст сохранён; попросите продолжить или сократите задачу.",
            value if value.starts_with("NATIVE_")&&value.ends_with("_PENDING")=>"Эта функция ещё не доступна в испытательном контуре. Запрос остановлен; действие не считается выполненным.",
            value if value.starts_with("PDF_")||value.starts_with("SOURCE_")=>"Не удалось прочитать подтверждённый источник. Проверьте доступность оригинала; ответ не подтверждён.",
            value if value.starts_with("TOOL_")=>"Инструмент не завершил действие. Откройте подробности; Лес не считает его выполненным.",
            _=>"Не удалось завершить запрос. Подробности сохранены в журнале. Проверьте подключение модели и повторите действие."
        };
        json!({"code":self.code,"detail":detail,"message":detail,"retryable":self.status!=StatusCode::UNPROCESSABLE_ENTITY})
    }
}
pub type Result<T> = std::result::Result<T, Failure>;
