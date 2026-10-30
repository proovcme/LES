"""Describe the evidence actually packed, without inferring answerability."""
from __future__ import annotations

from dataclasses import replace
from proxy.services.context_governor_service import ContextKind, ContextObject, conservative_token_estimate

COVERAGE_ID = 'request:indexed-section-coverage'


def section_coverage(sources, visible):
    groups = {}
    for source in sources:
        if not isinstance(source, dict) or not source.get('parent_id'):
            continue
        key = tuple(str(source.get(field) or '') for field in ('dataset_id', 'doc_name', 'parent_id'))
        counts = groups.setdefault(key, set())
        count = source.get('section_fragment_count')
        counts.add(count if type(count) is int and count > 0 else None)
    included = {}
    for source in visible:
        if not isinstance(source, dict) or not source.get('qdrant_point_id'):
            continue
        key = tuple(str(source.get(field) or '') for field in ('dataset_id', 'doc_name', 'parent_id'))
        included.setdefault(key, set()).add(str(source['qdrant_point_id']))
    rows = []
    for key, counts in groups.items():
        total = next(iter(counts)) if len(counts) == 1 and None not in counts else None
        count = len(included.get(key, set()))
        valid_total = total is not None and count <= total
        rows.append(dict(zip(('dataset_id', 'doc_name', 'parent_id'), key)) | {
            'indexed_fragments': total, 'model_fragments': count,
            'omitted_fragments': total-count if valid_total else None,
            'status': ('complete' if count == total else 'partial') if valid_total else 'unknown',
        })
    return {'schema': 'les.model-section-coverage.v1', 'sections': rows,
            'scope': 'indexed fragments only; completeness of the original, OCR and answerability are not established'}


def coverage_reservation(sources):
    report = model_coverage(sources, [])
    if not report['sections']:
        return None
    # Reserve before selecting evidence. Finalization changes only this object;
    # it cannot evict a fragment or reintroduce a dangling citation afterward.
    maximum = max(len(ContextObject(COVERAGE_ID, model_coverage(sources, values)).render())
                  for values in ([], sources))
    return ContextObject(COVERAGE_ID, ' ' * (maximum + 64 * len(report['sections'])))


def finalize_coverage(packet, sources, visible):
    report = ContextObject(COVERAGE_ID, model_coverage(sources, visible))
    sections, delta = [], 0
    for section in packet.sections:
        objects = []
        for obj in section.objects:
            if section.kind == ContextKind.REQUEST and obj.object_id == COVERAGE_ID:
                difference = conservative_token_estimate(report.render()) - conservative_token_estimate(obj.render())
                if difference > 0:
                    raise ValueError('SECTION_COVERAGE_RESERVATION_EXCEEDED')
                delta += difference
                objects.append(report)
            else:
                objects.append(obj)
        change = sum(conservative_token_estimate(new.render()) - conservative_token_estimate(old.render())
                     for new, old in zip(objects, section.objects))
        sections.append(replace(section, objects=tuple(objects), token_count=section.token_count + change))
    return replace(packet, sections=tuple(sections), included_tokens=packet.included_tokens + delta)


def model_coverage(sources, visible):
    """Reference existing source labels instead of repeating long filenames/paths."""
    report = section_coverage(sources, visible)
    for row in report['sections']:
        key = tuple(row.pop(field) for field in ('dataset_id', 'doc_name', 'parent_id'))
        row['source_indexes'] = [source['index'] for source in sources if isinstance(source, dict)
            and 'index' in source and tuple(str(source.get(field) or '') for field in ('dataset_id', 'doc_name', 'parent_id')) == key]
    return report
