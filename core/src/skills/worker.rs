//! CPython compatibility protocol only. Scheduling, permissions and persistence are Rust-owned.
pub const SCRIPT: &str = r#"
import contextlib, hashlib, json, os, sys
from pathlib import Path
protocol = sys.stdout
def send(value):
    encoded = json.dumps(value, ensure_ascii=False, allow_nan=False)
    if len(encoded) > 32000:
        raise ValueError('Skill result exceeds row budget')
    protocol.write(encoded + '\n'); protocol.flush()
try:
    # The parent sends this frame only after attaching the process lifetime/memory/CPU guard.
    initial = json.loads(sys.stdin.readline())
    # Embedded CPython may list ../runtime in its ._pth for the old application.
    # Retain the standard library and explicit dependency directories only;
    # never execute site/.pth or expose the LES backend to approved functions.
    base = Path(sys.base_prefix).resolve()
    version = f'python{sys.version_info.major}.{sys.version_info.minor}'
    stdlib = base / getattr(sys, 'platlibdir', 'lib') / version
    roots = {base, base / 'Lib', base / 'DLLs', stdlib, stdlib / 'lib-dynload',
             base / f'python{sys.version_info.major}{sys.version_info.minor}.zip',
             stdlib.parent / f'python{sys.version_info.major}{sys.version_info.minor}.zip',
             base / 'Lib' / 'site-packages', stdlib / 'site-packages'}
    dependencies = [Path(value).resolve() for value in initial['module_roots']]
    roots.update(dependencies)
    sys.path[:] = [str(Path(value).resolve()) for value in sys.path
                   if value and Path(value).resolve() in roots]
    for dependency in dependencies:
        if str(dependency) not in sys.path:
            sys.path.append(str(dependency))
    dll_handles = []
    if os.name == 'nt':
        for directory in initial.get('dll_roots', []):
            dll_handles.append(os.add_dll_directory(directory))
    if os.name != 'nt':
        import resource
        if sys.platform != 'darwin':
            resource.setrlimit(resource.RLIMIT_AS, (512 * 1024 * 1024,) * 2)
        resource.setrlimit(resource.RLIMIT_CPU, (15, 15))
    source = Path('script.py').read_bytes()
    if hashlib.sha256(source).hexdigest() != initial['sha256']:
        raise ValueError('Approved script hash mismatch')
    namespace = {'__name__': 'les_approved_skill'}
    with contextlib.redirect_stdout(sys.stderr):
        exec(compile(source, 'script.py', 'exec'), namespace)
    function = namespace[initial['entrypoint']]
    send({'ready': True})
    for line in sys.stdin:
        item = json.loads(line)
        try:
            with contextlib.redirect_stdout(sys.stderr):
                result = function(item['input'])
            response = {'id': item['id'], 'status': 'ok', 'result': result}
        except (ValueError, KeyError, TypeError, ArithmeticError) as error:
            response = {'id': item['id'], 'status': 'error', 'error': str(error)[:500]}
        send(response)
except BaseException as error:
    try:
        missing = isinstance(error, ModuleNotFoundError)
        send({'fatal': 'SKILL_DEPENDENCY_MISSING' if missing else 'SKILL_SCRIPT_FAILED',
              'exception': type(error).__name__, 'message': str(error)[:2000],
              'module': str(getattr(error, 'name', '') or '')[:128] if missing else ''})
    except BaseException:
        pass
    sys.exit(1)
"#;
