import json
from pathlib import Path
from zipfile import ZipFile
import pytest
from tools.skill_python_asset import stage,digest


def source(root):
    root.mkdir()
    (root/'python.exe').write_bytes(b'fixture executable')
    (root/'python313.dll').write_bytes(b'fixture DLL')
    (root/'LICENSE.txt').write_text('Python license',encoding='utf-8')
    with ZipFile(root/'python313.zip','w') as zipped:zipped.writestr('json/__init__.py','')
    (root/'python313._pth').write_text('python313.zip\n.\n../runtime\nimport site\n',encoding='utf-8')
    library=root/'Lib/site-packages';library.mkdir(parents=True)
    (library/'dependency.pth').write_text('raise RuntimeError("must not execute at startup")')
    package=library/'example';package.mkdir();(package/'__init__.py').write_text('value=1')
    metadata=library/'example-1.2.dist-info';metadata.mkdir();(metadata/'METADATA').write_text('Name: example\nVersion: 1.2\n')
    (metadata/'LICENSE').write_text('Dependency license')
    return root


def test_stage_preserves_dependencies_and_licenses_without_application_startup(tmp_path):
    original=source(tmp_path/'source');before=(original/'python313._pth').read_bytes()
    target=tmp_path/'payload/native/skill-python';receipt=stage(target,original,lock_sha256='a'*64)
    assert receipt['site_startup'] is False and receipt['application_import_path'] is False
    assert receipt['distributions']==[{'name':'example','version':'1.2'}]
    assert (target/'python313._pth').read_text()=='python313.zip\n.\nLib/site-packages\n'
    assert (original/'python313._pth').read_bytes()==before
    inventory=json.loads((target/'skill-python-files.json').read_text())
    assert all(digest(target/f['path'])==f['sha256'] for f in inventory['files'])
    assert (target/'Lib/site-packages/example-1.2.dist-info/LICENSE').read_text()=='Dependency license'
    assert str(original) not in json.dumps(receipt)
    with pytest.raises(FileExistsError):stage(target,original)


@pytest.mark.parametrize('path',['application.py','Lib/site-packages/backend/__init__.py','Lib/site-packages/proxy.py','data/private.json'])
def test_application_and_unexpected_roots_are_refused_before_creating_target(tmp_path,path):
    original=source(tmp_path/'source');extra=original/path;extra.parent.mkdir(parents=True,exist_ok=True);extra.write_text('not a dependency')
    target=tmp_path/'output'
    with pytest.raises(ValueError):stage(target,original)
    assert not target.exists()


def test_output_inside_source_and_incomplete_runtime_are_refused(tmp_path):
    original=source(tmp_path/'source')
    with pytest.raises(ValueError):stage(original/'nested',original)
    (original/'python313.dll').unlink()
    with pytest.raises(ValueError):stage(tmp_path/'output',original)
    assert not (tmp_path/'output').exists()


def test_prepared_skill_asset_can_be_staged_again(tmp_path):
    first=tmp_path/'first';stage(first,source(tmp_path/'source'))
    second=tmp_path/'second';receipt=stage(second,first)
    assert receipt['distributions']==[{'name':'example','version':'1.2'}]
    assert (second/'python313._pth').read_text()=='python313.zip\n.\nLib/site-packages\n'


def test_safe_dependency_paths_are_declared_without_executing_startup_hooks(tmp_path):
    original=source(tmp_path/'source');library=original/'Lib/site-packages'
    (library/'win32/lib').mkdir(parents=True);(library/'win32/lib/helper.py').write_text('value=1')
    (library/'pywin32_system32').mkdir();(library/'pywin32_system32/example.dll').write_bytes(b'fixture')
    (library/'vendor.pth').write_text('win32\nwin32\\lib\n../../runtime\nC:/outside\nimport forbidden_startup\n')
    target=tmp_path/'output';receipt=stage(target,original)
    assert receipt['module_roots']==['Lib/site-packages','Lib/site-packages/win32','Lib/site-packages/win32/lib']
    assert receipt['dll_roots']==['Lib/site-packages/pywin32_system32']
    assert (target/'python313._pth').read_text()=='python313.zip\n.\nLib/site-packages\n'


def test_compatibility_bootstrap_uses_the_same_rust_selector_and_refuses_bad_asset(tmp_path,monkeypatch):
    import os
    if not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Native selector asset required')
    from proxy.services import skill_compute_service as service
    root=tmp_path/'payload/runtime';root.mkdir(parents=True)
    target=root.parent/'native/skill-python'
    stage(target,source(tmp_path/'source'))
    monkeypatch.setattr(service,'__file__',str(root/'proxy/services/skill_compute_service.py'))
    monkeypatch.delenv('LES_SKILL_PYTHON',raising=False)
    assert Path(service._program())==target/'python.exe'
    (target/'python313._pth').write_text('python313.zip\n.\n../runtime\nimport site\n',encoding='utf-8')
    with pytest.raises(service.SkillExecutionError) as fault:service._program()
    assert fault.value.code=='SKILL_RUNTIME_INVALID'
