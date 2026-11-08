"""IO bridge only: a supervised Rust process parses spreadsheets and presentations."""
import logging
import hashlib
import math
import os
import time
from pathlib import Path
from backend import native_core
from backend.extraction_errors import DocumentExtractionError

logger = logging.getLogger(__name__)
SUFFIXES = {'.xlsx', '.xlsm', '.xls', '.csv', '.pptx'}


def prepare(path,data_dir,file_key,dataset_id,splitter,timings):
    if (Path(path).suffix.lower() not in SUFFIXES or not os.getenv('LES_NATIVE_FORMAT_WORKER')
            or not native_core.supports('document_dispatch')):
        return None
    from backend.document_router import DocumentRoute
    from backend import qdrant_support as support
    started=time.monotonic()
    try:
        asset=getattr(getattr(splitter,'_len',None),'asset',None)
        chunking={'unit':'chars' if getattr(splitter,'_len',len) is len else 'tokens',
            'chunk_size':splitter.chunk_size,'chunk_overlap':splitter.chunk_overlap,'tokenizer':asset}
        result=read(path,pipeline='nodes',data_dir=str(Path(data_dir).resolve()),file_key=file_key,
            dataset_id=dataset_id,chunking=chunking,minimum=support.MIN_CHUNK,
            final_minimum=support.FINAL_MIN_CHUNK,table_row_limit=support.TABLE_ROW_INDEX_MAX_CHUNKS)
        if (result.get('owner')!='rust.format-dispatch' or not isinstance(result.get('nodes'),list)
                or result.get('count')!=len(result['nodes'])):
            raise DocumentExtractionError('FORMAT_WORKER_RESPONSE_INVALID')
        return DocumentRoute(**result['route']),result['nodes']
    finally:
        timings['native_format_sec']=timings.get('native_format_sec',0)+time.monotonic()-started


def read(path, **settings):
    path = Path(path).resolve()
    if path.suffix.lower() not in SUFFIXES:
        return None
    program = os.getenv('LES_NATIVE_FORMAT_WORKER')
    if not program:
        return None  # Explicit compatibility with packages predating this component.
    if not native_core.supports('worker_documents'):
        raise DocumentExtractionError('DOCUMENT_CONVERTER_UNAVAILABLE')
    try:
        timeout = float(os.getenv('RAG_PARSE_FILE_TIMEOUT_SEC', '1800'))
        if not math.isfinite(timeout):
            timeout = 1800
        timeout = max(.1, min(3600, timeout))
    except ValueError:
        timeout = 1800
    request = {'schema':'les.document-convert.v1', 'path':str(path), **settings}

    def invoke():
        try:
            result = native_core.required('worker_documents', request={
                'schema':'les.worker-host.v1', 'program':program,
                'directory':str(Path(program).parent), 'args':['--serve-document-formats'],
                'context':'les.document-convert.v1', 'timeout_ms':int(timeout*1000), 'input':request,
            })
        except native_core.NativeCoreError as error:
            code = str(error)
            logger.warning('Rust document worker unavailable code=%s', code)
            raise DocumentExtractionError('DOCUMENT_CONVERTER_UNAVAILABLE' if code in {
                'WORKER_PROGRAM_INVALID', 'WORKER_DIRECTORY_INVALID', 'WORKER_START_FAILED',
                'CORE_ASSET_UNAVAILABLE', 'CORE_CAPABILITY_MISMATCH',
            } else code if code.startswith('WORKER_') else 'DOCUMENT_CONVERSION_FAILED') from error
        value = result.get('result', {})
        if result.get('status') != 'completed' or value.get('status') != 'completed':
            code = value.get('code') or result.get('code') or 'DOCUMENT_CONVERSION_FAILED'
            logger.warning('Rust document conversion failed code=%s pid=%s exit=%s diagnostics=%s',
                code, result.get('worker_pid'), result.get('exit_code'), str(result.get('diagnostics') or '')[-4000:])
            raise DocumentExtractionError(code)
        value = value.get('value')
        if not isinstance(value, dict) or not isinstance(value.get('text'), str):
            raise DocumentExtractionError('DOCUMENT_CONVERSION_FAILED')
        missing = sum(sheet.get('missing_formula_cache') or 0 for sheet in value.get('sheets', []))
        if missing:
            logger.warning('Rust spreadsheet reader: missing_formula_cache=%s; no values substituted', missing)
        return value

    try:
        return invoke()
    except DocumentExtractionError as error:
        if path.suffix.lower() != '.csv' or error.code != 'DOCUMENT_ENCODING_REQUIRED':
            raise
        # Keep the existing conservative legacy-encoding detector until its
        # confidence/ambiguity contract has a native owner. CSV parsing is Rust.
        from backend.text_decoding import decode_document_text
        data = path.read_bytes()
        request['decoded_text'] = decode_document_text(data)
        request['decoded_sha256'] = hashlib.sha256(data).hexdigest()
        return invoke()
