"""Batch lexical preparation; Unicode rules and the SQL schema remain unchanged."""
from collections import Counter
from functools import lru_cache
from backend import native_core
from backend.inference.lexical_tokens import CURRENT_TOKENIZER, _STOP_WORDS, normalized_text, normalized_words, stem_russian_word

_cached_stem = lru_cache(maxsize=32768)(stem_russian_word)


def python_counts(text):
    return Counter((_cached_stem(word) if len(word) <= 128 else stem_russian_word(word))
                   if len(word) >= 4 and word.isalpha() else word
                   for word in normalized_words(text) if word not in _STOP_WORDS)


def prepare(texts):
    if not native_core.available() or native_core.mode() == 'auto':
        # The paired load trial did not establish a native win over this cache.
        return [python_counts(text) for text in texts]
    normalized = [normalized_text(text) for text in texts]
    try:
        result = native_core.call('prepare_terms', tokenizer=CURRENT_TOKENIZER,
                                  texts=normalized, stop_words=sorted(_STOP_WORDS))
    except UnicodeEncodeError:
        # Surrogates are delimiters in the reference regex; preserve that behavior.
        result = native_core.call('prepare_terms', tokenizer=CURRENT_TOKENIZER,
                                  documents=[normalized_words(text) for text in texts],
                                  stop_words=sorted(_STOP_WORDS))
    documents = result.get('documents')
    if result.get('tokenizer') != CURRENT_TOKENIZER or not isinstance(documents, list) or len(documents) != len(texts):
        raise native_core.NativeCoreError('CORE_LEXICAL_RESPONSE_INVALID')
    for row in documents:
        terms = row.get('terms')
        if (not isinstance(terms, dict) or any(not isinstance(k, str) or type(v) is not int or v < 1 for k, v in terms.items())
                or type(row.get('length')) is not int or row['length'] != sum(terms.values())):
            raise native_core.NativeCoreError('CORE_LEXICAL_RESPONSE_INVALID')
    return [row['terms'] for row in documents]
