import pytest

from tools.evaluate_claim_judge import metrics
from tools.prepare_rag_trial_fixture import local_base


def test_false_accepts_and_unknowns_are_both_visible():
    rows = [{'expected': 'not_found', 'label': 'supported'},
            {'expected': 'contradicted', 'error': 'not_run'},
            {'expected': 'supported', 'label': 'supported'}]
    result = metrics(rows)
    assert result['false_accepts'] == 1
    assert result['false_accept_rate_lower'] == .5
    assert result['false_accept_rate_upper'] == 1
    assert result['accuracy'] == pytest.approx(1/3)


def test_no_judge_responses_cannot_be_reported_as_reliable():
    result = metrics([{'expected': 'not_found', 'error': 'admission'}])
    assert result['valid'] == 0
    assert result['false_accept_rate_upper'] == 1
    assert result['accuracy'] == 0


@pytest.mark.parametrize('url', ['https://127.0.0.1:1', 'http://example.com', 'http://name:secret@localhost:1'])
def test_fixture_trial_cannot_send_documents_to_remote_endpoint(url):
    with pytest.raises(ValueError): local_base(url)
