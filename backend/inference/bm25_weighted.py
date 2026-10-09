"""Versioned BM25 document weights for a separately rebuilt, frozen corpus.

IDF is applied exactly once by Qdrant. The corpus mean is measured at build
time; changing it requires rebuilding these sparse vectors, not query tuning.
The production TF encoder and its index are deliberately unchanged.
"""
from dataclasses import dataclass
from collections import Counter
import math

from backend.inference.bm25_sparse import tokenize, _term_id


@dataclass(frozen=True)
class BM25Profile:
    average_length: float
    k1: float = 1.2
    b: float = 0.75
    schema: str = "les.bm25.v2"

    def __post_init__(self):
        if self.schema != "les.bm25.v2":
            raise ValueError("Unknown sparse index profile")
        if not math.isfinite(self.average_length) or self.average_length <= 0:
            raise ValueError("Corpus average length must be positive and finite")
        if not math.isfinite(self.k1) or self.k1 <= 0 or not 0 <= self.b <= 1:
            raise ValueError("Invalid BM25 parameters")

    def document(self, text: str) -> dict[int, float]:
        terms = tokenize(text)
        counts = Counter(_term_id(term) for term in terms)
        norm = self.k1 * (1 - self.b + self.b * len(terms) / self.average_length)
        return {term: (self.k1 + 1) * tf / (tf + norm) for term, tf in counts.items()}

    def query(self, text: str) -> dict[int, float]:
        # Repeating a word in the question must not multiply its document TF.
        return {_term_id(term): 1.0 for term in tokenize(text)}

    @classmethod
    def from_documents(cls, texts):
        lengths = [len(tokenize(text)) for text in texts]
        if not lengths or not sum(lengths):
            raise ValueError("Cannot fit BM25 to an empty lexical corpus")
        return cls(sum(lengths) / len(lengths))
