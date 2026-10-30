"""Parity and failure boundaries for the native lexical preparation path."""
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import os
import random
import pytest

from backend import native_core
from backend.lexical_preparation import prepare
from backend.inference.lexical_tokens import tokenize_current


@pytest.fixture
def native(monkeypatch):
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Build the native core and set LES_CORE_LIBRARY for its integration gate')
    monkeypatch.setenv('LES_CORE_MODE', 'native')
    assert native_core.available()


def test_absent_asset_retains_python_path_and_explicit_mode_fails(tmp_path, monkeypatch):
    monkeypatch.setenv('LES_CORE_LIBRARY', str(tmp_path/'missing.dll'))
    monkeypatch.setenv('LES_CORE_MODE', 'auto')
    assert prepare(['Кабеля PE PE']) == [Counter(tokenize_current('Кабеля PE PE'))]
    monkeypatch.setenv('LES_CORE_MODE', 'native')
    with pytest.raises(native_core.NativeCoreError, match='CORE_ASSET_UNAVAILABLE'):
        prepare(['Кабеля'])


def test_corrupt_present_asset_is_rejected_before_loading(tmp_path, monkeypatch):
    path = tmp_path/'fake.dll'; path.write_bytes(b'not executable')
    (tmp_path/'core-receipt.json').write_text('{}')
    monkeypatch.setenv('LES_CORE_LIBRARY', str(path)); monkeypatch.setenv('LES_CORE_MODE','auto')
    with pytest.raises(native_core.NativeCoreError, match='CORE_ASSET_MISMATCH'):
        native_core.available()


def test_python_rollback_does_not_retain_unbounded_ocr_words(monkeypatch):
    from backend.lexical_preparation import _cached_stem
    monkeypatch.setenv('LES_CORE_MODE','python')
    _cached_stem.cache_clear()
    text = 'оборудование'*1000 + 'ого'
    assert prepare([text]) == [Counter(tokenize_current(text))]
    assert _cached_stem.cache_info().currsize == 0


def test_native_terms_match_python_across_scripts_unicode_and_suffixes(native):
    rng = random.Random(770)
    alphabet = 'абвгдежзийклмнопрстуфхцчшщьыэюяёabcdefghijklmnopqrstuvwxyzıİſK😀_/.−‑１２３'
    texts = ['', 'PE РЕ pe ＰＥ\u00a0СП 256.1325800.2016\tЩР‑2 BB_63',
             'кабеля кабель оборудования сечения земли для при их', 'е\u0308лка ² K İ ı ſ']
    texts += [' '.join(''.join(rng.choices(alphabet,k=rng.randrange(1,30))) for _ in range(30)) for _ in range(249)]
    assert prepare(texts) == [Counter(tokenize_current(text)) for text in texts]
    assert prepare(['кабел\ud800я']) == [Counter(tokenize_current('кабел\ud800я'))]


def test_native_calls_are_independent_across_threads(native):
    texts = ['Кабеля PE ' * 100] * 8
    with ThreadPoolExecutor(max_workers=4) as executor:
        outputs = list(executor.map(prepare, [[text] for text in texts]))
    assert all(value == [Counter(tokenize_current(texts[0]))] for value in outputs)


def test_auto_keeps_cached_lexical_path_after_loading_the_core(native, monkeypatch):
    monkeypatch.setenv('LES_CORE_MODE','auto')
    monkeypatch.setattr(native_core,'call',lambda *a,**k: pytest.fail('unmeasured automatic lexical switch'))
    assert prepare(['Кабеля PE']) == [Counter(tokenize_current('Кабеля PE'))]


def test_native_protocol_rejects_unknown_version_and_oversized_batch(native):
    with pytest.raises(native_core.NativeCoreError, match='CORE_TOKENIZER_MISMATCH'):
        native_core.call('prepare_terms', tokenizer='other', stop_words=[], documents=[])
    with pytest.raises(native_core.NativeCoreError, match='CORE_BATCH_LIMIT'):
        native_core.call('prepare_terms', tokenizer='les.lexical.v2', stop_words=[], documents=[[]]*257)
