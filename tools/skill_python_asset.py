"""Prepare the portable interpreter used by approved external Python skills."""
import argparse
from email.parser import BytesParser
import hashlib
import io
import json
import locale
import os
from pathlib import Path
import re
import shutil
from urllib.request import urlopen
from zipfile import ZipFile


CONFIG = Path(__file__).resolve().parents[1] / "config"


def _verified_archive(contract, variable, timeout):
    local_archive = os.getenv(variable, "").strip()
    if local_archive:
        archive = Path(local_archive)
        if not archive.is_file():
            raise FileNotFoundError(f"{variable} points to a missing archive: {archive}")
        data = archive.read_bytes()
    else:
        with urlopen(contract["archive_url"], timeout=timeout) as response:
            data = response.read()
    if hashlib.sha256(data).hexdigest() != contract["archive_sha256"].lower():
        raise ValueError(f"Archive SHA-256 mismatch for {variable}")
    return data


def stage_build_inputs(directory):
    """Stage the two pinned archives used to assemble the optional runtime."""
    python = json.loads((CONFIG / "windows_python.json").read_text(encoding="utf-8"))
    uv = json.loads((CONFIG / "windows_uv.json").read_text(encoding="utf-8"))
    if python.get("schema") != "les.windows-python.v2" or uv.get("schema") != "les.windows-uv.v1":
        raise ValueError("Unsupported Windows build input format")

    python_archive = directory / python["archive_name"]
    python_data = _verified_archive(python, "LES_WINDOWS_PYTHON_ARCHIVE", 120)
    uv_data = _verified_archive(uv, "LES_WINDOWS_UV_ARCHIVE", 60)
    with ZipFile(io.BytesIO(uv_data)) as archive:
        matches = [name for name in archive.namelist() if Path(name).name == uv["binary_name"]]
        if len(matches) != 1:
            raise ValueError("The uv archive must contain one executable")
        binary = archive.read(matches[0])
    if hashlib.sha256(binary).hexdigest() != uv["binary_sha256"].lower():
        raise ValueError("uv executable SHA-256 mismatch")

    executable = directory / uv["binary_name"]
    python_archive.write_bytes(python_data)
    executable.write_bytes(binary)
    return python_archive, executable


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream,'sha256').hexdigest()


def _link(path):
    return path.is_symlink() or (hasattr(path,'is_junction') and path.is_junction())


def stage(output, source, *, lock_sha256=''):
    source,output=Path(source).resolve(),Path(output).resolve()
    if output.exists():
        raise FileExistsError('Use a new skill runtime directory; rollback assets are never overwritten')
    if output.is_relative_to(source):
        raise ValueError('Skill output must be outside its source runtime')
    archives=list(source.glob('python*.zip'))
    if len(archives)!=1 or not re.fullmatch(r'python[0-9]{2,4}\.zip',archives[0].name):
        raise ValueError('A portable Windows Python standard library is required')
    version=archives[0].stem
    if not (source/'python.exe').is_file() or not (source/(version+'.dll')).is_file():
        raise ValueError('The portable Python executable and matching DLL are required')
    files=[]
    for directory, directories, names in os.walk(source,followlinks=False):
        root=Path(directory)
        for name in directories:
            if _link(root/name):
                raise ValueError('Skill runtime must not contain directory links')
        directories[:]=[name for name in directories if name!='__pycache__']
        for name in names:
            path=root/name
            if _link(path) or not path.is_file():
                raise ValueError('Skill runtime must contain only regular files')
            relative=path.relative_to(source)
            if len(relative.parts)==1 and name in {'skill-python-files.json','skill-python-receipt.json'}:
                continue
            if relative.suffix in {'.pyc','._pth'}:
                continue
            if len(relative.parts)==1:
                if relative.suffix.lower() not in {'.exe','.dll','.pyd','.zip','.txt','.cat'} and name.upper() not in {'LICENSE','COPYING'}:
                    raise ValueError('Unexpected file at the root of the portable runtime')
            elif relative.parts[0] not in {'Lib','DLLs'}:
                raise ValueError('Only standard library and dependency directories belong in the skill runtime')
            if relative.parts[:3] in {('Lib','site-packages','backend'),('Lib','site-packages','proxy')}:
                raise ValueError('LES application modules must not be bundled as skill dependencies')
            if relative.as_posix() in {'Lib/site-packages/backend.py','Lib/site-packages/proxy.py'}:
                raise ValueError('LES application modules must not be bundled as skill dependencies')
            files.append((path,relative))
    output.mkdir(parents=True)
    for path,relative in files:
        destination=output/relative
        destination.parent.mkdir(parents=True,exist_ok=True)
        shutil.copy2(path,destination)
    return seal(output, version, lock_sha256=lock_sha256)


def seal(output, version, *, lock_sha256=''):
    """Finish a trusted, freshly extracted interpreter in its destination."""
    library=output/'Lib/site-packages'
    library.mkdir(parents=True,exist_ok=True)
    module_roots=['Lib/site-packages']
    for pointer in sorted(library.glob('*.pth')):
        pointer_bytes=pointer.read_bytes()
        try:pointer_text=pointer_bytes.decode('utf-8-sig')
        except UnicodeDecodeError:pointer_text=pointer_bytes.decode(locale.getencoding())
        for raw in pointer_text.splitlines():
            line=raw.strip().replace('\\','/')
            if not line or line.startswith(('#','import ' ,'import\t')):
                continue
            relative=Path(line)
            if relative.is_absolute() or ':' in line or '..' in relative.parts:
                continue
            directory=(library/relative).resolve()
            if directory.is_dir() and directory.is_relative_to(library.resolve()):
                name=directory.relative_to(output).as_posix()
                if name not in module_roots:module_roots.append(name)
    if len(module_roots)>64:
        raise ValueError('Too many declared skill dependency paths')
    dll_roots=['Lib/site-packages/pywin32_system32'] if (library/'pywin32_system32').is_dir() else []
    (output/(version+'._pth')).write_text(version+'.zip\n.\nLib/site-packages\n',encoding='utf-8')
    distributions=[]
    for path in sorted((output/'Lib/site-packages').glob('*.dist-info/METADATA')):
        message=BytesParser().parsebytes(path.read_bytes(),headersonly=True)
        distributions.append({'name':message.get('Name',''),'version':message.get('Version','')})
    inventory={'schema':'les.skill-python-files.v1','files':[
        {'path':p.relative_to(output).as_posix(),'sha256':digest(p)}
        for p in sorted(output.rglob('*')) if p.is_file()]}
    inventory_path=output/'skill-python-files.json'
    inventory_path.write_text(json.dumps(inventory,ensure_ascii=False,indent=2),encoding='utf-8')
    receipt={'schema':'les.skill-python.v1','executable':'python.exe','python_sha256':digest(output/'python.exe'),
        'stdlib':version+'.zip','stdlib_sha256':digest(output/(version+'.zip')),
        'inventory':'skill-python-files.json','inventory_sha256':digest(inventory_path),
        'source_uv_lock_sha256':lock_sha256,'distributions':distributions,
        'module_roots':module_roots,'dll_roots':dll_roots,
        'site_startup':False,'application_import_path':False}
    (output/'skill-python-receipt.json').write_text(json.dumps(receipt,ensure_ascii=False,indent=2),encoding='utf-8')
    return receipt


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    options=parser.parse_args()
    result=stage(options.output,options.source)
    print(json.dumps({'schema':result['schema'],'distributions':len(result['distributions'])}))
