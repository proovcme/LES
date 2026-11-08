"""Successful checks do not retain test indexes; failed cases remain inspectable."""
import os
from pathlib import Path
import subprocess
import sys


def test_cleanup_runs_after_teardown_closes_sqlite_and_keeps_failed_cases(tmp_path):
    cases = tmp_path/'cases'; cases.mkdir()
    (cases/'conftest.py').write_bytes(Path(__file__).with_name('conftest.py').read_bytes())
    (cases/'test_cases.py').write_text('''
import sqlite3
import stat
import pytest
from pathlib import Path

@pytest.fixture
def database(tmp_path):
    db = sqlite3.connect(tmp_path/'test.db')
    db.execute('CREATE TABLE fixture(value)')
    yield db
    db.close()

def test_pass(tmp_path, database):
    (tmp_path/'readonly.txt').write_text('passed')
    (tmp_path/'readonly.txt').chmod(stat.S_IREAD)

def test_fail(tmp_path):
    (tmp_path/'failure.txt').write_text('keep')
    assert False, 'intentional fixture failure'

@pytest.fixture
def failed_teardown(tmp_path):
    yield
    raise RuntimeError('intentional teardown failure')

def test_teardown_error(tmp_path, failed_teardown):
    (tmp_path/'teardown.txt').write_text('keep')

def test_same_long_prefix_directory_first(tmp_path):
    Path(__file__).with_name('first-path.txt').write_text(str(tmp_path))

def test_same_long_prefix_directory_second(tmp_path):
    assert str(tmp_path) != Path(__file__).with_name('first-path.txt').read_text()
''', encoding='utf-8')
    scratch = tmp_path/'scratch'
    run = subprocess.run([sys.executable, '-m', 'pytest', str(cases), '-q', '--basetemp', str(scratch)],
        env={**os.environ, 'PYTEST_ADDOPTS': ''}, capture_output=True, timeout=45)
    assert run.returncode == 1, run.stdout.decode(errors='replace')
    assert not list(scratch.rglob('readonly.txt'))
    assert len(list(scratch.rglob('failure.txt'))) == 1
    assert len(list(scratch.rglob('teardown.txt'))) == 1
