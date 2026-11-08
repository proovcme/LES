"""Shared lexical rules, with an explicit vocabulary version for persisted vectors."""
import re
from backend import native_core

TOKEN_RE = re.compile(r"[0-9a-zа-яё.-]{2,}", re.IGNORECASE)
LEGACY_TOKENIZER = "les.lexical.v1"
CURRENT_TOKENIZER = "les.lexical.v2"

NO_STEM_WORDS = {
    "какие", "какой", "какая", "какое", "каких", "каким", "какими",
    "где", "смотреть", "требования", "нормы", "норма", "требование",
    "найти", "пункт", "раздел", "свод", "правил", "гост", "сп",
    "случаях", "случае", "случай", "выполнять", "выполнение", "делать",
    "допускается", "допускать", "почему", "зачем", "что", "кто", "как",
    "когда", "куда", "откуда",
    "нужно", "должно", "следует", "необходимо", "быть", "может", "можно", "ли",
    "или", "для", "при", "под", "над", "все", "всех", "всеми", "чем", "тем", "только"
}


def stem_russian_word(word: str) -> str:
    return native_core.required('lexical_tokens', request={'action': 'stem', 'word': word})['word']


# Separate words from punctuation, but keep document codes as whole terms.
# Do not transliterate lookalike Latin/Cyrillic letters: PE and РЕ are distinct.
_V2_RE = re.compile(r"[0-9a-zа-яё]+(?:[._/-][0-9a-zа-яё]+)*", re.IGNORECASE)
_SHORT_FUNCTION_WORDS = frozenset("а в и к с у о я на не но от по из до за об во со ко то же бы да он мы вы ты ей ее их".split())
_STOP_WORDS = (NO_STEM_WORDS - {"сп", "гост"}) | _SHORT_FUNCTION_WORDS
_HYPHENS = str.maketrans({"‐": "-", "‑": "-", "−": "-"})


def _text(text):
    return str(text or '').encode('utf-8', errors='replace').decode('utf-8')


def normalized_text(text):
    return native_core.required('unicode', request={'mode': 'lexical', 'texts': [_text(text)]})['texts'][0]


def _tokens(text, action):
    return native_core.required('lexical_tokens', request={'action': action, 'text': _text(text),
                                'stop_words': sorted(_STOP_WORDS)})['tokens']


def normalized_words(text):
    return _tokens(text, 'words')


def tokenize_current(text):
    return _tokens(text, 'tokens')


def compact_query_terms(text):
    return _tokens(text, 'compact')
