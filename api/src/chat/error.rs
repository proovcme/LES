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
            || code == "CHAT_RETRIEVAL_MODE_INVALID"
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
            "CHAT_RETRIEVAL_MODE_INVALID"=>"Выберите способ поиска в документах в параметрах ответа.",
            "TOOL_SCHEMA_INVALID"=>"Проверьте параметры инструмента по его описанию.",
            "TOOL_SCOPE_VIOLATION"=>"Документ или набор не входит в разрешённую область чтения.",
            "SOURCE_CONTINUATION_VIEW_REQUIRED" => "Для продолжения чтения передайте view_sha256 из предыдущей порции вместе с next_offset.",
            "SOURCE_VIEW_CHANGED" => "Изменился снимок прочитанного текста. Начните чтение заново; старые и новые порции нельзя объединять.",
            "SOURCE_READ_LIMIT" | "CONTEXT_SOURCE_LIMIT" => "Для одного чтения найдено слишком много текста. Уточните область поиска или читайте выбранный документ порциями; источники не были молча пропущены.",
            "SOURCE_ATOMIC_UNIT_OVERFLOW" => "Целая строка таблицы или абзац не помещается. Увеличьте max_chars; фрагмент не был обрезан.",
            "OCR_NO_TEXT" => "На странице не удалось распознать текст. Это не подтверждает отсутствие сведений в оригинале.",
            "OCR_UNAVAILABLE" => "Локальный OCR недоступен. Проверьте установленный обработчик распознавания; ответ по скану не подтверждён.",
            "SOURCE_CONTINUATION_REVISION_REQUIRED"=>"Для продолжения передайте index_revision из предыдущего ответа. Не объединяйте выдержки разных поколений индекса.",
            "SOURCE_INDEX_NOT_READY"=>"В выбранных наборах нет готовых поисковых фрагментов. Подготовьте документы к поиску; это не отсутствие ответа в оригиналах.",
            "INDEX_NOT_CONFIGURED"=>"Поиск по документам ещё не настроен. Подготовьте набор к поиску в разделе «Данные» и повторите вопрос.",
            "INDEX_STORAGE_UNAVAILABLE"=>"Поисковое хранилище недоступно. Проверьте состояние Леса и повторите вопрос после восстановления.",
            "PDF_PAGE_VIEW_INVALID"=>"Повреждена сохранённая структура страницы PDF. Повторно обработайте документ; ответ по этой странице не подтверждён.",
            "TABLE_FACTS_REQUIRED"|"TABLE_ROW_NOT_VISIBLE"=>"Не удалось связать числовой ответ с прочитанными ячейками. Ответ не подтверждён; уточните вопрос или повторите чтение.",
            "TABLE_VALUE_INVALID"|"TABLE_UNITS_MISMATCH"=>"Не удалось проверить расчёт по выбранным ячейкам: проверьте значения и единицы. Пустые ячейки не считаются нулём.",
            "TABLE_ANSWER_TOO_LARGE"=>"Выбрано слишком много табличных данных для одного ответа. Разделите запрос на части.",
            "MODEL_QUEUE_TIMEOUT"|"CHAT_SERVICE_BUSY"=>"Модель занята. Дождитесь завершения текущего запроса и повторите.",
            "CHAT_MEMORY_GUARD"=>"Недостаточно свободной оперативной памяти для модели. Освободите не менее 4 ГБ и повторите запрос.",
            "CONNECTION_ROLE_UNASSIGNED"|"ANSWER_MODEL_NOT_ASSIGNED"=>"Модель для чата не назначена. Подключите модель и выберите её для ответов в настройках.",
            "CONNECTION_DISABLED"=>"Подключение модели отключено. Включите его в настройках.",
            "CONNECTION_SECRET_MISSING"=>"Не найден ключ API модели. Проверьте подключение в настройках.",
            "CLOUD_CONSENT_REQUIRED"=>"Разрешите использование подключённой внешней модели в настройках.",
            "CLOUD_SOURCE_LOCAL_ONLY"|"CLOUD_SCOPE_UNVERIFIED"=>"Для этого набора передача внешней модели не разрешена или уровень доступа не подтверждён. Выберите локальную модель.",
            "CLOUD_EMBEDDING_SCOPE_UNVERIFIED"=>"Запрос поиска может содержать локальные данные. Для этого чата выберите локальную модель поиска.",
            "CLOUD_ATTACHMENT_LOCAL_ONLY"=>"Вложение нельзя передать внешней модели. Выберите локальную модель для работы с файлом.",
            "CLOUD_HISTORY_UNVERIFIED"=>"У истории этого диалога другой источник или модель. Начните новый диалог с внешней моделью либо продолжите этот на локальной.",
            "CLOUD_MEMORY_LOCAL_ONLY"|"CLOUD_SUMMARY_LOCAL_ONLY"=>"Личная память и сводка доступны только локальной модели.",
            "SESSION_PROVIDER_OVERRIDE_DISABLED"=>"Выберите модель в настройках Леса. Подключение модели на один запрос в этом режиме пока недоступно.",
            "CANDIDATE_ACCEPTANCE_REQUIRED"=>"Режим проверки кандидата модели здесь недоступен. Используйте настройки моделей.",
            "CAPABILITY_SNAPSHOT_STALE"|"CAPABILITY_SNAPSHOT_MISSING"|"CAPABILITY_SNAPSHOT_EXPIRED"=>"Обновите проверку подключения модели в настройках.",
            "UPSTREAM_TIMEOUT"|"MODEL_TIMEOUT"=>"Модель не ответила вовремя. Проверьте её состояние и повторите запрос.",
            "UPSTREAM_AUTHENTICATION_FAILED"=>"Сервис модели отклонил ключ API. Проверьте ключ в настройках подключения.",
            "UPSTREAM_ACCESS_DENIED"=>"Сервис запретил доступ к модели. Проверьте права подключения.",
            "UPSTREAM_MODEL_NOT_FOUND"=>"Сервис не нашёл назначенную модель. Проверьте её имя и адрес подключения.",
            "UPSTREAM_RATE_LIMITED"=>"Сервис модели ограничил число запросов. Подождите немного и повторите.",
            "UPSTREAM_SERVICE_UNAVAILABLE"=>"Сервис модели сейчас недоступен. Проверьте, запущен ли он, и повторите запрос.",
            "UPSTREAM_UNREACHABLE"|"UPSTREAM_STREAM_INTERRUPTED"=>"Связь с моделью прервалась. Проверьте сервер модели и повторите запрос.",
            "CONTEXT_REQUIRED_SECTION_OVERFLOW"=>"Выбранные документы и обязательные результаты не помещаются в контекст модели. Сузьте задачу или выберите модель с большим контекстом.",
            "INDEX_CHANGED_DURING_SEARCH"|"PDF_REVISION_CHANGED"|"SOURCE_REVISION_CHANGED"=>"Документ изменился во время чтения. Повторите запрос для текущей редакции.",
            "SOURCE_EVIDENCE_MISSING"=>"Лес не нашёл подтверждения в выбранных документах. Уточните вопрос или проверьте, что выбран нужный файл и его индекс готов.",
            "SOURCE_READ_REQUIRED"=>"Модель завершила поиск, но не дочитала документ для проверки ответа. Ответ не подтверждён; уточните задачу или повторите запрос.",
            "SOURCE_CITATION_MISSING"=>"Модель прочитала источники, но после повтора не связала ответ с доказательствами. Ответ не подтверждён; повторите запрос или уточните нужный документ.",
            "SOURCE_PART_IDENTITY_MISMATCH"=>"Имя рисунка или вложения не совпадает с его идентификатором. Откройте список частей документа и повторите чтение по точному имени. Ответ не подтверждён.",
            "SOURCE_PART_IDENTITY_AMBIGUOUS"=>"Одному имени соответствуют разные части документа. Уточните нужный рисунок или вложение по списку частей; ответ не подтверждён.",
            "SOURCE_PART_NOT_FOUND"=>"В этой редакции документа нет указанного рисунка или вложения. Откройте список частей и выберите нужное имя заново.",
            "CHAT_CANCELLED"=>"Ответ остановлен. Уже выполненные действия и прочитанная часть сохранены.",
            "CHAT_REQUEST_NOT_REPLAYABLE"=>"Этот запрос уже выполняется или был прерван. Автоматический повтор отключён, чтобы не повторить действия с файлами. Проверьте историю и отправьте новый запрос.",
            "IDEMPOTENCY_KEY_REUSED"=>"Ключ повторного запроса уже использован для другого сообщения. Отправьте сообщение с новым ключом.",
            "CHAT_DEADLINE_EXCEEDED"=>"Истекло время выполнения запроса. Проверьте историю и результаты действий; сузьте задачу или повторите запрос.",
            "MODEL_OUTPUT_LIMIT_REACHED"=>"Ответ достиг лимита длины и не завершён. Полученный текст сохранён; попросите продолжить или сократите задачу.",
            value if value.starts_with("NATIVE_")&&value.ends_with("_PENDING")=>"Эта функция ещё не доступна в испытательном контуре. Запрос остановлен; действие не считается выполненным.",
            value if value.starts_with("PDF_")||value.starts_with("SOURCE_")=>"Не удалось прочитать подтверждённый источник. Проверьте доступность оригинала; ответ не подтверждён.",
            value if value.starts_with("ATTACHMENT_")||value.starts_with("WORKBOOK_")||value.starts_with("SPREADSHEET_")||value.starts_with("DOCUMENT_")=>crate::attachments::detail(value),
            value if value.starts_with("TOOL_")=>"Инструмент не завершил действие. Откройте подробности; Лес не считает его выполненным.",
            _=>"Не удалось завершить запрос. Подробности сохранены в журнале. Проверьте подключение модели и повторите действие."
        };
        json!({"code":self.code,"detail":detail,"message":detail,"retryable":self.status!=StatusCode::UNPROCESSABLE_ENTITY})
    }
}
pub type Result<T> = std::result::Result<T, Failure>;
