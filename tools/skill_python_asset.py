"""Stage an offline skill-only interpreter from an already prepared portable Python.

No downloads or package installs. Preserve the source's dependency set and licenses.
Application source paths and automatic site/.pth startup are never added.
"""
import argparse
from email.parser import BytesParser
import hashlib
import json
import locale
import os
from pathlib import Path
import re
import shutil


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
        'stdlib':archives[0].name,'stdlib_sha256':digest(output/archives[0].name),
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
