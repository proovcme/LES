"""Browser navigation of the bundled forest view."""
import shutil
import subprocess


def test_actual_javascript_navigation():
    node = shutil.which('node')
    assert node
    subprocess.run([node, 'tests/navigation_controls.cjs'], check=True, capture_output=True, text=True)
