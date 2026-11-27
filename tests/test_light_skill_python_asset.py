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


def test_package_checks_the_lock_and_files_of_an_existing_skill_receipt(tmp_path):
    from tools.build_light_package import ROOT, hash_file, stage_skill_dependencies
    pinned = hash_file(ROOT/'uv.lock')
    old = tmp_path/'old'
    raw = source(tmp_path/'source')
    stage(old, raw, lock_sha256='different')
    target = tmp_path/'package/skill-python'
    with pytest.raises(ValueError, match='different lock'):
        stage_skill_dependencies(target, old)
    assert not target.exists()
    sealed = tmp_path/'sealed'
    stage(sealed, raw, lock_sha256=pinned)
    assert stage_skill_dependencies(tmp_path/'good', sealed)['source_uv_lock_sha256'] == pinned
    (sealed/'python313.dll').write_bytes(b'changed')
    with pytest.raises(ValueError, match='sealed files'):
        stage_skill_dependencies(target, sealed)
    assert not target.exists()


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


def test_windows_payload_pins_separate_python_and_keeps_native_pdfium(tmp_path):
    from tools.build_light_package import skill_asset_path, stage_pdfium, stage_skill_asset
    from tools.light_license_audit import stage_skill_notices

    runtime = tmp_path / 'runtime'
    stage(runtime, source(tmp_path / 'source'))
    site = runtime / 'Lib/site-packages'
    pdfium = site / 'pypdfium2_raw/pdfium.dll'
    pdfium.parent.mkdir()
    pdfium.write_bytes(b'PDFium from the pinned wheel')
    distribution = site / 'pypdfium2-5.8.0.dist-info'
    license_path = distribution / 'licenses/BUILD_LICENSES/pdfium.txt'
    license_path.parent.mkdir(parents=True)
    license_path.write_text('PDFium license', encoding='utf-8')
    (distribution / 'METADATA').write_text('Name: pypdfium2\nVersion: 5.8.0\n', encoding='utf-8')
    (distribution / 'RECORD').write_text(
        'pypdfium2-5.8.0.dist-info/licenses/BUILD_LICENSES/pdfium.txt,,\n', encoding='utf-8'
    )

    payload = tmp_path / 'payload'
    stage_pdfium(payload, pdfium)
    stage_skill_notices(runtime)
    stage_skill_asset(payload, runtime, {'build_number': 955})
    component = skill_asset_path(payload, 955)
    descriptor = json.loads((payload / 'native/skill-python-download.json').read_text())
    assert (payload / 'native/pdf/pdfium.dll').read_bytes() == pdfium.read_bytes()
    assert list((payload / 'THIRD_PARTY_NOTICES/native/pdfium').rglob('pdfium.txt'))
    assert not list(payload.rglob('python.exe'))
    assert not list(payload.rglob('*.zip'))
    assert descriptor['bytes'] == component.stat().st_size
    assert descriptor['sha256'] == digest(component)
    assert descriptor['receipt_sha256'] == digest(runtime / 'skill-python-receipt.json')
    with ZipFile(component) as archive:
        assert {'python.exe', 'skill-python-receipt.json', 'THIRD_PARTY_NOTICES/Python-LICENSE.txt'} <= set(archive.namelist())


def test_build_inputs_check_both_archives_and_uv_binary(tmp_path, monkeypatch):
    from tools import skill_python_asset as assets

    config = tmp_path / "contracts"
    config.mkdir()
    monkeypatch.setattr(assets, "CONFIG", config)
    python_zip = tmp_path / "python-source.zip"
    uv_zip = tmp_path / "uv-source.zip"
    with ZipFile(python_zip, "w") as archive:
        archive.writestr("python.exe", b"portable interpreter")
    with ZipFile(uv_zip, "w") as archive:
        archive.writestr("uv-x86_64/uv.exe", b"pinned uv")
    monkeypatch.setenv("LES_WINDOWS_PYTHON_ARCHIVE", str(python_zip))
    monkeypatch.setenv("LES_WINDOWS_UV_ARCHIVE", str(uv_zip))

    expected_uv = tmp_path / "expected-uv.exe"
    expected_uv.write_bytes(b"pinned uv")
    python_contract = {
        "schema": "les.windows-python.v2", "archive_name": "python.zip",
        "archive_sha256": digest(python_zip),
    }
    uv_contract = {
        "schema": "les.windows-uv.v1", "binary_name": "uv.exe",
        "archive_sha256": digest(uv_zip), "binary_sha256": digest(expected_uv),
    }
    (config / "windows_python.json").write_text(json.dumps(python_contract), encoding="utf-8")
    uv_config = config / "windows_uv.json"
    uv_config.write_text(json.dumps(uv_contract), encoding="utf-8")
    output = tmp_path / "output"
    output.mkdir()
    staged_python, staged_uv = assets.stage_build_inputs(output)
    assert staged_python == output / "python.zip"
    assert staged_python.read_bytes() == python_zip.read_bytes()
    assert staged_uv == output / "uv.exe"
    assert staged_uv.read_bytes() == expected_uv.read_bytes()

    refused = tmp_path / "refused"
    refused.mkdir()
    uv_config.write_text(json.dumps({**uv_contract, "binary_sha256": "0" * 64}), encoding="utf-8")
    with pytest.raises(ValueError, match="uv executable SHA-256"):
        assets.stage_build_inputs(refused)
    uv_zip.write_bytes(b"modified")
    with pytest.raises(ValueError, match="LES_WINDOWS_UV_ARCHIVE"):
        assets.stage_build_inputs(refused)
    python_zip.write_bytes(b"modified")
    with pytest.raises(ValueError, match="LES_WINDOWS_PYTHON_ARCHIVE"):
        assets.stage_build_inputs(refused)
    assert list(refused.iterdir()) == []
