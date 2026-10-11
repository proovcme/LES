"""Worker for explicitly approved, hash-pinned, pure JSON skill functions.

This is a resource-bounded trusted-script worker, not an arbitrary-code sandbox.
"""
import hashlib
import json
import os
from pathlib import Path
import sys


def main():
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from backend.light_processes import attach_lifetime_job
    job = attach_lifetime_job(memory_bytes=512 * 1024 * 1024, process_limit=1, cpu_seconds=15)
    if os.name != 'nt':
        import resource
        if sys.platform != "darwin":
            resource.setrlimit(resource.RLIMIT_AS, (512 * 1024 * 1024,) * 2)
        # macOS does not implement RLIMIT_AS; the parent enforces RSS.
        resource.setrlimit(resource.RLIMIT_CPU, (15, 15))
    request = json.loads(Path('input.json').read_text(encoding='utf-8'))
    source = Path('script.py').read_bytes()
    if hashlib.sha256(source).hexdigest() != request['sha256']:
        raise ValueError('Approved script hash mismatch')
    namespace = {'__name__': 'les_approved_skill'}
    exec(compile(source, 'script.py', 'exec'), namespace)
    function = namespace[request['entrypoint']]
    for index, item in enumerate(request['items']):
        try:
            result = {'id': item['id'], 'status': 'ok', 'result': function(item['input'])}
        except (ValueError, KeyError, TypeError, ArithmeticError) as error:
            result = {'id': item['id'], 'status': 'error', 'error': str(error)[:500]}
        content = json.dumps(result, ensure_ascii=False, allow_nan=False)
        if len(content) > 32000:
            raise ValueError('Skill result exceeds row budget')
        temporary = Path(f'{index:04d}.tmp')
        with temporary.open('w', encoding='utf-8') as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        temporary.replace(f'{index:04d}.json')
    # Keep Windows job alive until process exit.
    _ = job


if __name__ == '__main__':
    main()
