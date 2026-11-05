"""Thin local bridge; Rust owns the reusable worker, deadlines and descendants."""
import atexit
import logging
import math
import os
from pathlib import Path
import sys
import time

from backend import native_core
from backend.document_router import DocumentRoute
from backend.extraction_errors import MESSAGES as EXTRACTION_MESSAGES

logger = logging.getLogger(__name__)

_MESSAGES = {
    **EXTRACTION_MESSAGES,
    'WORKER_TIMEOUT': 'Чтение документа заняло слишком много времени. Обработчик остановлен; повторите обработку файла.',
    'WORKER_QUEUE_TIMEOUT': 'Обработчик документов занят. Повторите обработку позже.',
    'WORKER_CANCELLED': 'Чтение документа прервано. Повторите обработку файла.',
    'WORKER_OUTPUT_LIMIT': 'Документ дал слишком большой результат. Добавьте его отдельными частями.',
    'FORMAT_READ_FAILED': 'Не удалось прочитать документ. Проверьте, открывается ли исходный файл; подробности сохранены в журнале.',
    'FORMAT_WORKER_UNAVAILABLE': 'Обработчик документов отсутствует. Восстановите установку Леса.',
}


class FormatProcessError(RuntimeError):
    def __init__(self, code, exit_code=None):
        self.code = code
        self.exit_code = exit_code
        super().__init__(_MESSAGES.get(code,
            'Обработчик документов завершился с ошибкой. Повторите обработку файла; подробности сохранены в журнале.'))


def enabled():
    mode = os.getenv('LES_FORMAT_WORKER_MODE', 'auto').strip().lower()
    if mode not in {'auto', 'inline', 'native'}:
        raise FormatProcessError('FORMAT_WORKER_MODE_INVALID')
    if mode == 'inline':
        return False
    if native_core.supports('worker_run'):
        return True
    if mode == 'native':
        raise FormatProcessError('FORMAT_WORKER_UNAVAILABLE')
    return False


def prepare(file_path, data_dir, file_key, dataset_id, splitter, timings, allow_ocr):
    from backend.rag_config import embedding_model_id

    root = Path(__file__).resolve().parents[1]
    chunking = {
        'unit': 'chars' if getattr(splitter, '_len', len) is len else 'tokens',
        'chunk_size': splitter.chunk_size, 'chunk_overlap': splitter.chunk_overlap,
        'embedding_model': embedding_model_id(),
    }
    try:
        timeout = max(.1, min(3600., float(os.getenv('RAG_PARSE_FILE_TIMEOUT_SEC', '1800')) * .95))
    except ValueError:
        timeout = 1710.
    if not math.isfinite(timeout):
        timeout = 1710.
    request = {
        'schema': 'les.worker-host.v1',
        'program': str(Path(sys.executable).resolve()),
        # Embedded Python's ._pth isolates it from cwd/PYTHONPATH. Pass the
        # packaged runtime explicitly, just as the API launcher does.
        'args': ['-B', '-c',
            'import runpy,sys;sys.path.insert(0,sys.argv[1]);runpy.run_module("tools.light_document_worker",run_name="__main__")',
            str(root)],
        'directory': str(root), 'context': chunking, 'timeout_ms': int(timeout * 1000),
        'input': {
            'schema': 'les.document-format.v1', 'source': str(Path(file_path).resolve()),
            'data_dir': str(Path(data_dir).resolve()), 'file_key': file_key,
            'dataset_id': dataset_id, 'chunking': chunking, 'allow_ocr': allow_ocr,
        },
    }
    started = time.monotonic()
    try:
        response = native_core.call('worker_run', request=request)
    except native_core.NativeCoreError as error:
        if str(error).startswith('WORKER_'):
            raise FormatProcessError(str(error)) from error
        raise
    finally:
        timings['worker_total_sec'] = timings.get('worker_total_sec', 0) + time.monotonic() - started
    if response.get('status') != 'completed':
        logger.error('Format worker failed: code=%s pid=%s exit=%s diagnostics=%s',
                     response.get('code'), response.get('worker_pid'), response.get('exit_code'),
                     str(response.get('diagnostics') or '')[-8000:])
        raise FormatProcessError(str(response.get('code') or 'FORMAT_WORKER_FAILED'), response.get('exit_code'))
    result = response.get('result')
    if not isinstance(result, dict):
        raise FormatProcessError('FORMAT_WORKER_RESPONSE_INVALID')
    if result.get('status') != 'completed':
        logger.warning('Format operation failed: code=%s type=%s pid=%s diagnostics=%s',
            result.get('code'), result.get('error_type'), response.get('worker_pid'),
            str(result.get('diagnostics') or response.get('diagnostics') or '')[-8000:])
        raise FormatProcessError(str(result.get('code') or 'FORMAT_READ_FAILED'))
    route, nodes = result.get('route'), result.get('nodes')
    if (not isinstance(route, dict) or not isinstance(route.get('metadata'), dict)
            or nodes is not None and (not isinstance(nodes, list) or len(nodes) > 200000
                or any(not isinstance(node, dict) or not isinstance(node.get('text'), str)
                       or not isinstance(node.get('payload', {}), dict) for node in nodes))):
        raise FormatProcessError('FORMAT_WORKER_RESPONSE_INVALID')
    measured = result.get('timings', {})
    if not isinstance(measured, dict):
        raise FormatProcessError('FORMAT_WORKER_RESPONSE_INVALID')
    for key, value in measured.items():
        if isinstance(value, (int, float)) and math.isfinite(value) and value >= 0:
            timings[key] = timings.get(key, 0) + value
    logger.info('Format worker: pid=%s reused=%s', response.get('worker_pid'), response.get('reused'))
    return DocumentRoute(**route), nodes


def stop():
    try:
        if native_core.supports('worker_stop'):
            native_core.call('worker_stop')
    except (native_core.NativeCoreError, OSError):
        pass


atexit.register(stop)
