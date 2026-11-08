"""Thin chat adapter for revision-checked native physical page reads."""
from backend import native_core
from backend.interface import Chunk
from proxy.services.chat_section_context_service import SectionReadError


def require_source_read_success(payload):
    outcome = native_core.required('harness_evidence', request={
        'action': 'source_read_outcome', 'result': payload,
    })
    if not outcome['allowed']:
        raise SectionReadError(422, detail={'code': outcome['code']})


def physical_page_chunks(payload):
    if payload.get('status') != 'ok':
        return ()
    value = payload['result']
    identity = f"pdf:{value['doc_id']}:{value['source_sha256']}:{value['page']}:{value['offset']}"
    meta = {key: value[key] for key in ('dataset_id', 'doc_id', 'doc_name', 'page', 'source_sha256')}
    meta.update(source_ref=f"{value['doc_name']}#page={value['page']}", source_page=value['page'],
                source_type='pdf_physical_page', atomic_evidence=True,
                chunk_id=identity, context_origin='pdf_physical_page',
                file_name=value['doc_name'], physical_page_complete=value['complete'])
    return (Chunk(value['text'], value['doc_id'], value['doc_name'], 1.0, meta),)


def require_complete_pdf_reads(results):
    outcome = native_core.required('harness_evidence', request={
        'action': 'pdf_read_coverage', 'results': results,
    })
    if not outcome['complete']:
        raise SectionReadError(422, detail={'code': outcome.get('code') or 'PDF_PAGE_INCOMPLETE'})
    return outcome


def pending_pdf_read(results):
    return native_core.required('harness_evidence', request={
        'action': 'pdf_read_coverage', 'results': results,
    }).get('continuation')
