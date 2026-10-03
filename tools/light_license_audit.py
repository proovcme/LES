"""Collect actual bundled Python notices and reject retired incompatible readers."""
from __future__ import annotations
import argparse
import importlib.metadata as metadata
import json
from pathlib import Path
import re
import shutil
import subprocess

BLOCKED = {'pymupdf', 'pymupdf4llm', 'pymupdf-layout', 'extract-msg', 'pcodedmp', 'oletools'}


def collect(site: Path, output: Path):
    distributions = sorted(metadata.distributions(path=[str(site)]), key=lambda d: d.metadata['Name'].lower())
    inventory = []
    for distribution in distributions:
        name, version = distribution.metadata['Name'], distribution.version
        if name.lower() in BLOCKED:
            raise ValueError(f'Retired dependency in release: {name}')
        destination = output / re.sub(r'[^a-zA-Z0-9_.-]', '_', name)
        destination.mkdir(parents=True, exist_ok=True)
        meta = distribution.metadata
        copied = []
        for file in distribution.files or []:
            parts = Path(str(file)).parts
            if not any(re.search(r'(?i)(licen[sc]e|copying|notice|copyright|authors)([._-]|$)', part) for part in parts):
                continue
            source = Path(distribution.locate_file(file)).resolve()
            if not source.is_relative_to(site.resolve()) or not source.is_file():
                continue
            relative = Path(*[part for part in parts if part not in ('.', '..')])
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            copied.append(target.relative_to(output).as_posix())
        classifiers = [v for v in meta.get_all('Classifier') or [] if v.startswith('License ::')]
        (destination / 'METADATA.txt').write_text(distribution.read_text('METADATA') or '', encoding='utf-8')
        inventory.append({'name': name, 'version': version,
                          'license': str(meta.get('License-Expression') or meta.get('License') or ''),
                          'license_classifiers': classifiers, 'notices': copied,
                          'source': f'https://pypi.org/project/{name}/{version}/#files'})
    (output / 'python-packages.json').write_text(json.dumps(inventory, ensure_ascii=False, indent=2), encoding='utf-8')
    (output / 'README.txt').write_text(
        'Third-party components retain their original licenses; the LES license does not replace them.\n'
        'Python source distributions: exact version links in python-packages.json.\n'
        'Pure-Python MPL components are supplied in source form in python/Lib/site-packages.\n'
        'Native component sources, including orjson, are available through those versioned source links.\n', encoding='utf-8')
    return inventory


def stage_notices(root: Path, payload: Path):
    output = payload / 'THIRD_PARTY_NOTICES'
    collect(payload / 'python/Lib/site-packages', output / 'python')
    shutil.copy2(payload / 'python/LICENSE.txt', output / 'Python-LICENSE.txt')
    shutil.copytree(root / 'licenses', output / 'native', dirs_exist_ok=True)
    result = subprocess.run(['cargo', 'metadata', '--locked', '--format-version', '1',
        '--filter-platform', 'x86_64-pc-windows-msvc', '--manifest-path',
        str(root / 'desktop/light/src-tauri/Cargo.toml')], capture_output=True, check=True, encoding='utf-8')
    cargo = json.loads(result.stdout)
    resolved = {node['id'] for node in cargo['resolve']['nodes']}
    packages = []
    for package in cargo['packages']:
        if package['id'] not in resolved or package['name'] == 'les-light': continue
        source = Path(package['manifest_path']).parent
        directory = output / 'rust' / f"{package['name']}-{package['version']}"
        directory.mkdir(parents=True, exist_ok=True)
        for path in source.rglob('*'):
            if path.is_file() and re.search(r'(?i)(licen[sc]e|copying|notice|copyright)', path.name):
                target = directory / path.relative_to(source)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, target)
        packages.append({'name':package['name'], 'version':package['version'], 'license':package['license'],
                         'source':f"https://crates.io/crates/{package['name']}/{package['version']}"})
    (output / 'rust-packages.json').write_text(json.dumps(packages,indent=2),encoding='utf-8')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--site', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    print(f'Collected notices for {len(collect(args.site, args.output))} Python packages')
