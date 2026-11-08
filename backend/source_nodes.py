"""Text/passport IO for Rust source-node preparation; no parsing or splitting."""
import logging

from backend import native_core
from backend.extraction_errors import DocumentExtractionError, MESSAGES

logger = logging.getLogger(__name__)


def prepare(text, *, dataset_id, file_key, route=None, pdf=False,
            size, overlap, minimum=100, final_minimum=20, unit='chars', tokenizer=None,
            payload_type='markdown', passport_reader=None):
    if not native_core.supports('document_source_nodes'):
        raise DocumentExtractionError('DOCUMENT_SOURCE_NODES_UNAVAILABLE')
    nodes = []
    cursor = 0

    def exchange(operation, value):
        nonlocal cursor
        if operation == 0:
            if value.get('offset') != cursor or value.get('limit') != 262144:
                raise DocumentExtractionError('DOCUMENT_SOURCE_NODES_INVALID')
            part = text[cursor:cursor+262144]
            cursor += len(part)
            return {'text':part,'complete':cursor>=len(text)}
        if operation == 1:
            batch = value.get('nodes')
            if not isinstance(batch,list) or len(nodes)+len(batch)>50000:
                raise DocumentExtractionError('DOCUMENT_SOURCE_NODES_INVALID')
            nodes.extend(batch)
            return {'ok':True}
        if operation == 2 and passport_reader is not None:
            try:
                return {'passports':passport_reader(value['pages'])}
            except Exception:
                # Optional contour enrichment must not erase readable page text.
                logger.warning('PDF page passport enrichment failed',exc_info=True)
                return {'passports':{}}
        raise DocumentExtractionError('DOCUMENT_SOURCE_NODES_INVALID')

    request = {
        'schema':'les.document-source-nodes.v1','kind':'pdf' if pdf else 'markdown',
        'dataset_id':dataset_id,'file_key':file_key,'size':size,'overlap':overlap,
        'minimum':minimum,'final_minimum':final_minimum,'unit':unit,'tokenizer':tokenizer,
        'payload_type':payload_type,'route_metadata':dict(getattr(route,'metadata',{}) or {}),
        'ocr':getattr(route,'pipeline','')=='markdown_needs_ocr','passports':passport_reader is not None,
    }
    try:
        result = native_core.run_source_nodes(request,exchange)
    except native_core.NativeCoreError as error:
        code = str(error)
        logger.warning('Source node preparation failed code=%s kind=%s',code,request['kind'])
        raise DocumentExtractionError(code if code in MESSAGES else 'DOCUMENT_SOURCE_NODES_INVALID') from error
    if result.get('status')!='completed' or result.get('count')!=len(nodes):
        raise DocumentExtractionError('DOCUMENT_SOURCE_NODES_INVALID')
    return nodes
