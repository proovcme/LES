"""Exercise the real pre-push hook against a disposable local Git remote."""

from pathlib import Path
import os
import shutil
import subprocess

import pytest


@pytest.mark.skipif(not shutil.which("git") or not shutil.which("make"), reason="Git and Make required")
def test_pre_push_checks_the_outgoing_commit_and_blocks_failed_tests(tmp_path):
    work = tmp_path / "working copy"
    remote = tmp_path / "remote.git"
    work.mkdir()
    remote.mkdir()
    hook_variables = subprocess.check_output(
        ["git", "rev-parse", "--local-env-vars"], encoding="utf-8",
    ).splitlines()
    local_remote_env = {name: value for name, value in os.environ.items() if name not in hook_variables}

    def git(*args, cwd=work, smoke_status=0):
        return subprocess.run(
            ["git", "-c", "commit.gpgsign=false", *args],
            cwd=cwd,
            env={**local_remote_env, "LES_TEST_SMOKE_STATUS": str(smoke_status)},
            capture_output=True,
            encoding="utf-8",
            errors="replace",
            timeout=30,
        )

    git("init", "-q", "-b", "main").check_returncode()
    git("init", "-q", "--bare", cwd=remote).check_returncode()
    git("config", "--local", "user.name", "Fixture").check_returncode()
    git("config", "--local", "user.email", "fixture@example.invalid").check_returncode()
    git("config", "--local", "core.hooksPath", ".githooks").check_returncode()
    git("remote", "add", "fixture", str(remote)).check_returncode()
    hook = work / ".githooks" / "pre-push"
    hook.parent.mkdir()
    shutil.copyfile(Path(__file__).resolve().parents[1] / ".githooks" / "pre-push", hook)
    hook.chmod(0o755)
    (work / "Makefile").write_text(
        "pre-push:\n\t$(info running-smoke)\n"
        "ifneq ($(LES_TEST_SMOKE_STATUS),0)\n\t$(error planned smoke failure)\nendif\n",
        encoding="utf-8",
    )
    git("add", ".").check_returncode()
    git("commit", "-qm", "Test setup").check_returncode()
    source = work / "tracked.txt"
    source.write_text("committed", encoding="utf-8")
    git("add", "tracked.txt").check_returncode()
    git("commit", "-qm", "Candidate").check_returncode()
    head = git("rev-parse", "HEAD").stdout.strip()

    source.write_text("uncommitted", encoding="utf-8")
    interrupted = git("push", "fixture", "HEAD:refs/heads/checked")
    assert interrupted.returncode and "clean checkout" in interrupted.stderr
    git("restore", "tracked.txt").check_returncode()

    older = git("push", "fixture", "HEAD~:refs/heads/checked")
    assert older.returncode and "outgoing commit checked out" in older.stderr

    failed = git("push", "fixture", "HEAD:refs/heads/checked", smoke_status=1)
    assert failed.returncode and "running-smoke" in failed.stdout
    assert git("show-ref", "--verify", "refs/heads/checked", cwd=remote).returncode

    passed = git("push", "fixture", "HEAD:refs/heads/checked", "HEAD:refs/heads/also-checked")
    passed.check_returncode()
    assert passed.stdout.count("running-smoke") == 1
    assert f"Testing outgoing commit {head}" in passed.stdout
    assert git("rev-parse", "refs/heads/checked", cwd=remote).stdout.strip() == head
