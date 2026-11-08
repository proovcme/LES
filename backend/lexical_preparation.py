"""Retained batch facade; versioned Unicode, tokenization and counts are owned by Rust."""
from backend import native_core
from backend.inference.lexical_tokens import CURRENT_TOKENIZER, _STOP_WORDS


def prepare(texts):
    # A lone Python surrogate has no UTF-8 representation; it remains a token delimiter.
    raw = [str(text or '').encode('utf-8', errors='replace').decode('utf-8') for text in texts]
    result = native_core.required('prepare_terms', tokenizer=CURRENT_TOKENIZER,
                                  raw_texts=raw, stop_words=sorted(_STOP_WORDS))
    return [row['terms'] for row in result['documents']]
