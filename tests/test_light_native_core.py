"""Parity and failure boundaries for the native lexical preparation path."""
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import os
import random
import pytest

from backend import native_core
from backend.lexical_preparation import prepare
from backend.inference.lexical_tokens import tokenize_current
from oracles.lexical_819 import tokenize_current as oracle_tokenize


@pytest.fixture
def native(monkeypatch):
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Build the native core and set LES_CORE_LIBRARY for its integration gate')
    monkeypatch.setenv('LES_CORE_MODE', 'native')
    assert native_core.available()


def test_absent_asset_never_falls_back_to_python(tmp_path, monkeypatch):
    monkeypatch.setenv('LES_CORE_LIBRARY', str(tmp_path/'missing.dll'))
    monkeypatch.setenv('LES_CORE_MODE', 'auto')
    with pytest.raises(native_core.NativeCoreError, match='CORE_ASSET_UNAVAILABLE'):
        prepare(['Кабеля PE PE'])
    monkeypatch.setenv('LES_CORE_MODE', 'native')
    with pytest.raises(native_core.NativeCoreError, match='CORE_ASSET_UNAVAILABLE'):
        prepare(['Кабеля'])


def test_corrupt_present_asset_is_rejected_before_loading(tmp_path, monkeypatch):
    path = tmp_path/'fake.dll'; path.write_bytes(b'not executable')
    (tmp_path/'core-receipt.json').write_text('{}')
    monkeypatch.setenv('LES_CORE_LIBRARY', str(path)); monkeypatch.setenv('LES_CORE_MODE','auto')
    with pytest.raises(native_core.NativeCoreError, match='CORE_ASSET_MISMATCH'):
        native_core.available()


def test_legacy_python_flag_does_not_reintroduce_an_ocr_word_cache(native, monkeypatch):
    monkeypatch.setenv('LES_CORE_MODE','python')
    text = 'оборудование'*1000 + 'ого'
    assert prepare([text]) == [Counter(oracle_tokenize(text))]


def test_native_terms_match_python_across_scripts_unicode_and_suffixes(native):
    rng = random.Random(770)
    alphabet = 'абвгдежзийклмнопрстуфхцчшщьыэюяёabcdefghijklmnopqrstuvwxyzıİſK😀_/.−‑１２３'
    texts = ['', 'PE РЕ pe ＰＥ\u00a0СП 256.1325800.2016\tЩР‑2 BB_63',
             'кабеля кабель оборудования сечения земли для при их', 'е\u0308лка ² K İ ı ſ']
    texts += [' '.join(''.join(rng.choices(alphabet,k=rng.randrange(1,30))) for _ in range(30)) for _ in range(249)]
    assert prepare(texts) == [Counter(oracle_tokenize(text)) for text in texts]
    assert prepare(['кабел\ud800я']) == [Counter(oracle_tokenize('кабел\ud800я'))]


def test_native_calls_are_independent_across_threads(native):
    texts = ['Кабеля PE ' * 100] * 8
    with ThreadPoolExecutor(max_workers=4) as executor:
        outputs = list(executor.map(prepare, [[text] for text in texts]))
    assert all(value == [Counter(tokenize_current(texts[0]))] for value in outputs)


def test_auto_uses_the_mandatory_owner_without_optional_accelerator(native, monkeypatch):
    monkeypatch.setenv('LES_CORE_MODE','auto')
    monkeypatch.setattr(native_core,'call',lambda *a,**k: pytest.fail('unmeasured automatic lexical switch'))
    assert prepare(['Кабеля PE']) == [Counter(oracle_tokenize('Кабеля PE'))]


def test_native_protocol_rejects_unknown_version_and_oversized_batch(native):
    with pytest.raises(native_core.NativeCoreError, match='CORE_TOKENIZER_MISMATCH'):
        native_core.call('prepare_terms', tokenizer='other', stop_words=[], documents=[])
    with pytest.raises(native_core.NativeCoreError, match='CORE_BATCH_LIMIT'):
        native_core.call('prepare_terms', tokenizer='les.lexical.v2', stop_words=[], documents=[[]]*257)
