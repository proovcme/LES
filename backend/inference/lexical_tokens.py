"""Shared lexical rules, with an explicit vocabulary version for persisted vectors."""
import re
import unicodedata

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
    """A simple, robust Russian stemmer to handle common inflections."""
    if not re.match(r"^[а-яё]+$", word):
        return word
    endings = (
        "иями", "ям", "ыми", "ейший", "ейшая", "ейшее", "ейшие", "ейших",
        "ого", "его", "ому", "ему", "ыми", "ими", "ых", "их", "ою", "ею",
        "ая", "яя", "ое", "ее", "ые", "ие", "ым", "им", "ом", "ем", "ах", "ях",
        "ов", "ев", "ей", "ам", "ям", "ит", "ет", "ут", "ют", "ат", "ят", "ти",
        "а", "ев", "ов", "е", "и", "й", "о", "у", "ы", "ь", "я", "ю", "ию"
    )
    for ending in endings:
        if word.endswith(ending) and len(word) - len(ending) >= 4:
            return word[:-len(ending)]
    return word


# Separate words from punctuation, but keep document codes as whole terms.
# Do not transliterate lookalike Latin/Cyrillic letters: PE and РЕ are distinct.
_V2_RE = re.compile(r"[0-9a-zа-яё]+(?:[._/-][0-9a-zа-яё]+)*", re.IGNORECASE)
_SHORT_FUNCTION_WORDS = frozenset("а в и к с у о я на не но от по из до за об во со ко то же бы да он мы вы ты ей ее их".split())
_STOP_WORDS = (NO_STEM_WORDS - {"сп", "гост"}) | _SHORT_FUNCTION_WORDS
_HYPHENS = str.maketrans({"‐": "-", "‑": "-", "−": "-"})


def normalized_words(text):
    """Case and compatibility normalization shared by queries and documents."""
    source = unicodedata.normalize("NFKC", str(text or "")).translate(_HYPHENS)
    return _V2_RE.findall(source.casefold().replace("ё", "е"))


def tokenize_current(text):
    result = []
    for token in normalized_words(text):
        if token in _STOP_WORDS:
            continue
        if len(token) >= 4 and token.isalpha():
            token = stem_russian_word(token)
        result.append(token)
    return result


def compact_query_terms(text):
    """Exact FTS terms previously discarded: short labels and compound codes."""
    return [token for token in normalized_words(text) if token not in _STOP_WORDS
            and (len(token) < 3 or "_" in token or "/" in token)]
