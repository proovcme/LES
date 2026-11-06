from types import SimpleNamespace

from backend.interface import Chunk
from proxy.services.lexical_index_service import RetrievalTrace
from proxy.services.retrieval_quality_service import evaluate_retrieval_quality


def assess(question, *chunks):
    return evaluate_retrieval_quality(question=question, chunks=list(chunks),
                                      trace=RetrievalTrace(mode="dense_bm25_hybrid", score_kind="rrf"),
                                      kot=SimpleNamespace(ambiguous=False))


def test_file_name_alone_does_not_count_as_evidence():
    result = assess("Срок испытания?", Chunk("Паспорт оборудования", "1", "Срок испытания.pdf", .97, {}))
    assert result.status == "weak"
    assert result.detail == "no_source_text_support"
    assert result.term_coverage == 0
    assert result.payload()["answerability"] == "unverified"


def test_related_fragments_do_not_confirm_an_answer():
    result = assess("Какой срок испытания?",
                    Chunk("Испытания проводят перед сдачей.", "1", "Первый.pdf", .97, {}),
                    Chunk("Срок подачи документов указан в разделе 7.", "2", "Второй.pdf", .92, {}))
    assert result.status == "candidates" and not result.retry_wider
    assert result.payload()["answerability"] == "unverified"
    assert assess("Какой срок испытания?").payload()["answerability"] == "no_evidence_found"
