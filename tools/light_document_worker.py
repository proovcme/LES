"""Local format/OCR adapter. No chat, embedding or index publication happens here."""
from dataclasses import asdict
import faulthandler
import json
import logging
import os
from pathlib import Path
import sys
import traceback

LIMIT = 8 * 1024 * 1024


def convert(request):
    if request.get('schema') != 'les.document-format.v1':
        raise ValueError('FORMAT_SCHEMA_INVALID')
    source = Path(request['source'])
    data_dir = Path(request['data_dir'])
    key = Path(request['file_key'])
    if not source.is_absolute() or not data_dir.is_absolute() or key.is_absolute() or '..' in key.parts:
        raise ValueError('FORMAT_PATH_INVALID')
    if type(request.get('allow_ocr')) is not bool:
        raise ValueError('FORMAT_REQUEST_INVALID')

    from backend import qdrant_support as support
    from backend.qdrant_ingestion import QdrantIngestion
    from backend.qdrant_nodes import QdrantNodes
    from backend.rag_config import embedding_model_id, token_length_fn

    chunking = request['chunking']
    size, overlap = chunking['chunk_size'], chunking['chunk_overlap']
    if (type(size) is not int or type(overlap) is not int
            or not 1 <= size <= 1_000_000 or not 0 <= overlap < size):
        raise ValueError('FORMAT_CHUNKING_INVALID')
    if chunking['embedding_model'] != embedding_model_id():
        raise ValueError('FORMAT_CHUNKING_CHANGED')
    length = token_length_fn() if chunking['unit'] == 'tokens' else None
    if chunking['unit'] not in {'chars', 'tokens'} or chunking['unit'] == 'tokens' and length is None:
        raise ValueError('FORMAT_TOKENIZER_UNAVAILABLE')
    splitter = support.StructureAwareSplitter(chunk_size=size, chunk_overlap=overlap, len_fn=length)
    timings = {}
    route, nodes = QdrantIngestion._convert_file_inline(
        QdrantNodes(), source, data_dir, request['file_key'], request['dataset_id'],
        support.MarkdownNodeParser(), splitter, timings, request['allow_ocr'],
    )
    if nodes is not None and not nodes:
        from backend.extraction_errors import DocumentExtractionError
        raise DocumentExtractionError('NO_SEARCHABLE_TEXT')
    return {'status': 'completed', 'route': asdict(route), 'nodes': nodes, 'timings': timings}


def main():
    # Rust already provides a killable boundary for routing, parsing and OCR.
    os.environ['RAG_CONVERT_SUBPROCESS_ENABLED'] = 'false'
    faulthandler.enable()
    logging.basicConfig(level=logging.WARNING, stream=sys.stderr)
    # Keep the protocol separate even from C extensions writing directly to fd 1.
    protocol = os.fdopen(os.dup(sys.stdout.fileno()), 'wb')
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    while True:
        line = sys.stdin.buffer.readline(LIMIT + 1)
        if not line:
            return
        if len(line) > LIMIT or not line.endswith(b'\n'):
            raise ValueError('FORMAT_INPUT_LIMIT')
        envelope = json.loads(line)
        try:
            result = convert(envelope['input'])
        except Exception as error:
            logging.exception('Document format operation failed')
            result = {
                'status': 'failed',
                'code': (getattr(error, 'code', None) or ('FORMAT_UNSUPPORTED_SOURCE'
                         if type(error).__name__ == 'UnsupportedIndexingSourceError' else 'FORMAT_READ_FAILED')),
                'error_type': type(error).__name__,
                'diagnostics': traceback.format_exc(limit=12)[-8000:],
            }
        payload = json.dumps(
            {'request_id': envelope['request_id'], 'result': result},
            ensure_ascii=False, allow_nan=False, separators=(',', ':'),
        )
        protocol.write(payload.encode('utf-8') + b'\n')
        protocol.flush()


if __name__ == '__main__':
    main()
