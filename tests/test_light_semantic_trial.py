"""Evaluation accounting must not turn missing evidence into a success."""
import json
from pathlib import Path

import pytest

from tools.evaluate_light_semantics import retrieval_metrics, validate_fixture


def test_unanswerable_is_excluded_from_ranking_score():
    assert retrieval_metrics(['irrelevant'], []) == {'recall5': None, 'ndcg5': None}


def test_multiple_evidence_and_duplicate_chunks_do_not_inflate_score():
    result = retrieval_metrics(['a', 'a', 'noise', 'b'], ['a', 'b'])
    assert result['recall5'] == 1
    assert 0 < result['ndcg5'] < 1
    assert retrieval_metrics(['a'], ['a', 'b'])['recall5'] == .5


def test_authored_fixture_has_disjoint_ids_and_explicit_no_answer_cases():
    fixture = json.loads((Path(__file__).parent / 'fixtures/rag/semantic-v1.json').read_text())
    docs, cases = validate_fixture(fixture)
    assert len(docs) == 22 and len(cases) == 20
    assert {c['category'] for c in cases} == {'versions', 'exceptions', 'conflicts', 'unanswerable', 'direct'}
    assert all(not c['gold_documents'] for c in cases if c['category'] == 'unanswerable')
    fixture['cases'][0]['gold_documents'] = ['absent']
    with pytest.raises(ValueError, match='gold document'):
        validate_fixture(fixture)
