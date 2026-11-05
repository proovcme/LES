"""Stable extraction failures, never searchable substitute document text."""
MESSAGES = {
    'NO_SEARCHABLE_TEXT':'В документе не найден текст для поиска. Проверьте содержимое файла и распознавание сканов.',
    'OCR_DISABLED':'В документе есть страницы без текста. Включите локальное распознавание и повторите обработку.',
    'OCR_READ_FAILED':'Не удалось распознать страницы. Проверьте язык распознавания Windows и исходный документ.',
    'DOC_ENCRYPTED':'Документ Word защищён паролем. Добавьте незашифрованную копию.',
    'DOC_VERSION_UNSUPPORTED':'Эта версия старого формата Word не поддерживается. Сохраните копию в DOCX.',
    'DOC_TRUNCATED':'Структура документа Word повреждена или обрезана. Проверьте исходный файл.',
}


class DocumentExtractionError(RuntimeError):
    def __init__(self, code):
        self.code = code
        super().__init__(MESSAGES.get(code, 'Не удалось прочитать структуру документа. Проверьте исходный файл.'))
