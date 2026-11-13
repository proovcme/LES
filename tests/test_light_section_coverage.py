import json

import pytest

from proxy.services.chat_evidence_context import govern_inference_messages, model_visible_source_map
from proxy.services.context_governor_service import ContextKind, conservative_token_estimate
from backend import native_core
from tests.test_context_governor_service import _preset

COVERAGE_ID = 'request:indexed-section-coverage'


def section_coverage(sources, visible):
    return native_core.required('chat_context', request={
        'action': 'section_coverage', 'sources': sources, 'visible': visible})['value']


def source(index, point, total=2, **extra):
    return {'index': index, 'qdrant_point_id': point, 'dataset_id': 'ds', 'doc_name': 'Условия с пробелами.pdf',
            'parent_id': 'section-1', 'section_fragment_count': total, **extra}


def coverage_object(packet):
    return next(obj for section in packet.sections for obj in section.objects if obj.object_id == COVERAGE_ID)


def test_actual_model_packet_exposes_dropped_exception_without_claiming_answerability():
    sources = [source(1, 'rule'), source(2, 'exception')]
    rule = '[Источник 1]\nОсновное требование.'
    exception = '[Источник 2]\nИсключение при особых условиях. ' + 'x'*4000
    messages, packet = govern_inference_messages(preset=_preset(limit=1200, generation=200, safety=100),
        profile_prefix='Use sources.', request_payload='Какие условия?', evidence=[rule, exception], source_map=sources)
    visible = model_visible_source_map(packet, sources)
    report = coverage_object(packet).payload
    assert [s['qdrant_point_id'] for s in visible] == ['rule']
    assert report['sections'][0]['status'] == 'partial'
    assert report['sections'][0]['omitted_fragments'] == 1
    assert json.dumps(report, ensure_ascii=False, sort_keys=True, separators=(',', ':')) in str(messages)
    assert packet.included_tokens <= packet.input_budget_tokens
    assert not any(' '*50 in str(message) for message in messages)  # Reservation never leaves the packer.
    assert 'Исключение при' not in str(messages)


def test_complete_means_all_indexed_fragments_and_counts_duplicate_ids_once():
    sources = [source(1, 'rule'), source(2, 'exception'), source(3, 'exception')]
    assert section_coverage(sources, sources)['sections'][0] == {
        'dataset_id': 'ds', 'doc_name': 'Условия с пробелами.pdf', 'parent_id': 'section-1',
        'indexed_fragments': 2, 'model_fragments': 2, 'omitted_fragments': 0, 'status': 'complete'}
    _, packet = govern_inference_messages(preset=_preset(limit=2000), profile_prefix='p', request_payload='q',
        evidence=['[Источник 1]\nRule.', '[Источник 2]\nException.'], source_map=sources)
    assert coverage_object(packet).payload['sections'][0]['status'] == 'complete'
    # Finalization updates accounting but does not run selection again.
    actual = sum(conservative_token_estimate(obj.render()) for section in packet.sections for obj in section.objects)
    assert actual <= packet.included_tokens <= actual + 20


@pytest.mark.parametrize('counts', [(None, None), (2, 3), (True, 2), (-1, 2)])
def test_inconsistent_or_missing_counts_never_claim_complete(counts):
    sources = [source(1, 'a', counts[0]), source(2, 'b', counts[1])]
    assert section_coverage(sources, sources)['sections'][0]['status'] == 'unknown'


def test_document_coverage_counts_different_page_parents_as_one_reading_unit():
    sources=[source(i,f'page-{i}',parent_id=f'parent-{i}',context_unit='document:revision-A',
                    document_fragment_count=3) for i in (1,2,3)]
    complete=section_coverage(sources,sources)['sections']
    partial=section_coverage(sources,sources[:1])['sections']
    assert len(complete)==len(partial)==1
    assert complete[0]['indexed_fragments']==complete[0]['model_fragments']==3
    assert complete[0]['status']=='complete'
    assert partial[0]['status']=='partial' and partial[0]['omitted_fragments']==2


def test_two_documents_with_same_parent_id_do_not_share_coverage():
    first, other = source(1, 'a', 1), source(2, 'b', 1, doc_name='Другой.pdf')
    rows = section_coverage([first, other], [first])['sections']
    assert [r['status'] for r in rows] == ['complete', 'partial']


def test_legacy_sources_without_parent_have_no_invented_coverage():
    _, packet = govern_inference_messages(preset=_preset(), profile_prefix='p', request_payload='q',
        evidence=['[Источник 1]\nShort'], source_map=[{'index': 1}])
    assert all(obj.object_id != COVERAGE_ID for section in packet.sections for obj in section.objects)
