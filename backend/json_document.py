"""IO bindings for Rust JSON parsing; legacy decoding remains explicit."""
import logging
from pathlib import Path

from backend import native_core
from backend.extraction_errors import DocumentExtractionError, MESSAGES

logger = logging.getLogger(__name__)


def read(path, *, max_chars):
    path = Path(path).resolve()
    if not native_core.supports('document_json_stream'):
        raise DocumentExtractionError('JSON_READER_UNAVAILABLE')
    chunks = []
    decoded = [None]
    cursor = [0]

    def exchange(operation, value):
        if operation == 0:
            text = value.get('text')
            if not isinstance(text,str):
                raise DocumentExtractionError('JSON_READ_FAILED')
            chunks.append(text)
            return {'ok':True}
        if operation != 1 or value.get('offset') != cursor[0] or value.get('limit') != 262144:
            raise DocumentExtractionError('JSON_READ_FAILED')
        if decoded[0] is None:
            from backend.text_decoding import read_document_text
            try:
                decoded[0] = read_document_text(path)
            except ValueError as error:
                raise DocumentExtractionError('JSON_ENCODING_UNCERTAIN') from error
        part = decoded[0][cursor[0]:cursor[0]+262144]
        cursor[0] += len(part)
        return {'text':part,'complete':cursor[0]>=len(decoded[0])}

    try:
        result = native_core.run_json_document({
            'schema':'les.json-source.v1','path':str(path),
            'json_lines':path.suffix.lower()=='.jsonl','max_chars':max_chars,
        },exchange)
    except native_core.NativeCoreError as error:
        code = str(error)
        logger.warning('JSON reader failed code=%s',code)
        raise DocumentExtractionError(code if code in MESSAGES else 'JSON_READ_FAILED') from error
    if result.get('status') != 'completed':
        raise DocumentExtractionError(str(result.get('code') or 'JSON_READ_FAILED'),
            line=result.get('line'),column=result.get('column'))
    text = ''.join(chunks)
    if result.get('characters') != len(text):
        raise DocumentExtractionError('JSON_READ_FAILED')
    logger.info('JSON reader engine=%s records=%s truncated=%s legacy_numbers=%s',
                result.get('extractor'),result.get('records'),result.get('truncated'),result.get('legacy_numbers'))
    return text
