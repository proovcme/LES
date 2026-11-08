"""Independent Python Unicode oracle is a test tool; product normalization runs in Rust."""
import random
import unicodedata
from backend import native_core


def test_unicode_151_nfkc_and_casefold_match_the_former_authority():
    assert unicodedata.unidata_version == '15.1.0'
    texts = []
    for code in range(0x110000):
        if 0xD800 <= code <= 0xDFFF:
            continue
        char = chr(code)
        if unicodedata.normalize('NFKD', char) != char or char.casefold() != char or unicodedata.combining(char):
            texts.append(char)
    rng = random.Random(820)
    marks = [chr(n) for n in range(0x300, 0x370)]
    texts += ['A' + ''.join(rng.choices(marks, k=16)) + 'Ё e\u0301각' for _ in range(256)]
    texts += ['\u1100\u1161\u11A8', '\u0301\u0323', '\u212B\u0301', 'ＳＰ № ２５６ Straße\u00a0ЁЛКА ‑ PE РЕ']
    for offset in range(0, len(texts), 512):
        batch = texts[offset:offset + 512]
        value = native_core.required('unicode', request={'mode': 'nfkc', 'texts': batch})
        assert value['version'] == '15.1.0'
        assert value['texts'] == [unicodedata.normalize('NFKC', text) for text in batch]
        assert native_core.required('unicode', request={'mode': 'casefold', 'texts': batch})['texts'] == [text.casefold() for text in batch]
