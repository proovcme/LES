"""Contrast of the actual shared Light palette; runnable without GUI dependencies."""
import runpy
import unittest
from pathlib import Path


def luminance(color):
    channels = [int(color[i:i + 2], 16) / 255 for i in (1, 3, 5)]
    linear = [c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in channels]
    return sum(c * weight for c, weight in zip(linear, (0.2126, 0.7152, 0.0722)))


class LightPaletteTests(unittest.TestCase):
    def test_body_secondary_text_and_source_links_are_readable_on_surfaces(self):
        styles = runpy.run_path(str(Path(__file__).resolve().parents[1] / "sovushka/styles.py"))
        for theme_name in ("_DARK_THEME", "_LIGHT_THEME"):
            theme = styles[theme_name]
            for foreground in ("--text", "--dim", "--accent"):
                for background in ("--bg", "--bg-panel", "--bg-mod"):
                    with self.subTest(theme=theme_name, foreground=foreground, background=background):
                        values = sorted((luminance(theme[foreground]), luminance(theme[background])))
                        self.assertGreaterEqual((values[1] + 0.05) / (values[0] + 0.05), 4.5)


if __name__ == "__main__":
    unittest.main()
