"""Build and stage the locked static UI; Node is a build dependency only."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
FRONTEND = Path('desktop/light/frontend')
INPUTS = ('package.json', 'package-lock.json', 'tsconfig.json', 'vite.config.ts', 'index.html')


def source_digest(root):
    directory = root / FRONTEND
    paths = [directory / name for name in INPUTS]
    paths += sorted((directory / 'src').rglob('*'))
    digest = hashlib.sha256()
    for path in paths:
        if path.is_symlink():
            raise ValueError('Symlinks are not frontend build inputs')
        if path.is_dir() or path.name.endswith('.test.ts'):
            continue
        digest.update(path.relative_to(directory).as_posix().encode() + b'\0' + path.read_bytes() + b'\0')
    return digest.hexdigest()


def file_digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def npm_command():
    npm = shutil.which('npm.cmd') or shutil.which('npm')
    if not npm:
        raise RuntimeError('Node.js/npm is required on the build machine, not on user installations')
    return npm


def build(root=ROOT):
    directory = root / FRONTEND
    npm = npm_command()
    subprocess.run([npm, 'ci', '--ignore-scripts', '--no-fund', '--no-audit'], cwd=directory, check=True)
    subprocess.run([npm, 'run', 'build'], cwd=directory, check=True)
    dist = directory / 'dist'
    notices = []
    for name in ('svelte', 'marked', 'dompurify'):
        dependency = directory / 'node_modules' / name
        metadata = json.loads((dependency / 'package.json').read_text(encoding='utf-8'))
        license_file = next((path for path in dependency.iterdir() if path.name.upper() in {'LICENSE', 'LICENSE.MD', 'LICENSE.TXT'}), None)
        if license_file is None:
            raise RuntimeError(f'Frontend license missing: {name}')
        notices.append(f"{name} {metadata['version']}\n\n{license_file.read_text(encoding='utf-8')}\n")
    (dist / 'THIRD_PARTY_NOTICES.txt').write_text('\n'.join(notices), encoding='utf-8')
    files = {path.relative_to(dist).as_posix(): file_digest(path) for path in sorted(dist.rglob('*')) if path.is_file()}
    receipt = {'schema': 'les.frontend.v1', 'source_sha256': source_digest(root), 'files': files}
    (dist / 'build-receipt.json').write_text(json.dumps(receipt, indent=2) + '\n', encoding='utf-8')
    return receipt


def stage(root, runtime):
    directory = root / FRONTEND
    # Old fixture/minimal source trees have no frontend, and remain packageable.
    if not (directory / 'package.json').is_file():
        return
    dist = directory / 'dist'
    if dist.is_symlink() or not dist.resolve().is_relative_to(directory.resolve()):
        raise ValueError('Unsafe frontend build directory')
    try:
        if (dist / 'build-receipt.json').is_symlink():
            raise ValueError('Unsafe frontend receipt')
        receipt = json.loads((dist / 'build-receipt.json').read_text(encoding='utf-8'))
    except (OSError, ValueError) as error:
        raise RuntimeError('Build the static UI first: python tools/build_light_frontend.py') from error
    if receipt.get('schema') != 'les.frontend.v1' or receipt.get('source_sha256') != source_digest(root):
        raise RuntimeError('Frontend build is stale. Rebuild before packaging.')
    listed = receipt['files']
    actual = {path.relative_to(dist).as_posix() for path in dist.rglob('*') if path.is_file()}
    if actual != set(listed) | {'build-receipt.json'} or 'index.html' not in listed:
        raise ValueError('Unexpected frontend build files')
    for name, expected in listed.items():
        relative = Path(name)
        path = dist / relative
        if relative.is_absolute() or '..' in relative.parts or path.is_symlink() or not path.resolve().is_relative_to(dist.resolve()):
            raise ValueError('Unsafe frontend build asset')
        if path.suffix not in {'.html', '.js', '.css', '.svg', '.woff2', '.txt'} or path.stat().st_size > 5_000_000:
            raise ValueError('Unsupported frontend build asset')
        if file_digest(path) != expected:
            raise ValueError('Frontend build asset hash mismatch')
    target = runtime / FRONTEND / 'dist'
    target.mkdir(parents=True, exist_ok=True)
    for name in sorted(actual):
        destination = target / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(dist / name, destination)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--test', action='store_true', help='Run frontend tests without rebuilding')
    args = parser.parse_args()
    if args.test:
        subprocess.run([npm_command(), 'test'], cwd=ROOT / FRONTEND, check=True)
    else:
        receipt = build()
        print(json.dumps({'schema': receipt['schema'], 'files': len(receipt['files'])}))
