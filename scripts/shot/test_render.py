"""Font discovery tests for render.py; stdlib only: python3 -m unittest (from scripts/shot)."""
import os
import subprocess
import sys
import tempfile
import types
import unittest
from unittest import mock

try:
    import PIL  # noqa: F401
except ImportError:  # render.py exits without Pillow; font discovery doesn't need it
    pil = types.ModuleType("PIL")
    pil.Image = pil.ImageDraw = pil.ImageFont = types.ModuleType("PIL.stub")
    sys.modules["PIL"] = pil

import render

FONT = "/usr/share/fonts/TTF/DejaVuSansMono.ttf"
ASCII = "20-7e a0-17f"
NEEDED = ASCII + " 2500 256d 25b8"  # the box-drawing glyphs, none of the extras
WANTED = NEEDED + " 21b3 2922 2b13 273b"


def fonts(*rows):
    """fc-match -s output: one (path, spacing, charset) row per font, best ranked first."""
    return "".join("%s\t%s\t%s\n" % row for row in rows)


def ran(returncode=0, stdout="", stderr=""):
    return subprocess.CompletedProcess([], returncode, stdout, stderr)


class DefaultFont(unittest.TestCase):
    def discover(self, menlo=False, which="/usr/bin/fc-match", run=None):
        with mock.patch("os.path.exists", side_effect=lambda p: menlo and p == render.MENLO), \
             mock.patch("shutil.which", return_value=which) as w, \
             mock.patch("subprocess.run", **({"side_effect": run} if isinstance(run, BaseException) else {"return_value": run})) as r:
            return render.default_font(), w, r

    def exits(self, **kw):
        with self.assertRaises(SystemExit) as e:
            self.discover(**kw)
        self.assertIn("pass --font <path>", str(e.exception.code))
        return str(e.exception.code)

    def test_menlo_wins_when_present(self):
        found, which, run = self.discover(menlo=True, run=ran(stdout=fonts((FONT, "100", WANTED))))
        self.assertEqual(found, render.MENLO)
        which.assert_not_called()
        run.assert_not_called()

    def test_fontconfig_result_used_on_linux(self):
        found, _, run = self.discover(run=ran(stdout=fonts((FONT, "100", WANTED))))
        self.assertEqual(found, FONT)
        args = run.call_args[0][0]
        self.assertEqual(args[0], "/usr/bin/fc-match")
        self.assertIn("-s", args)
        self.assertIn("monospace:charset=" + render.WANTED_GLYPHS, args)

    def test_prefers_a_font_with_every_glyph(self):
        found, _, _ = self.discover(run=ran(stdout=fonts(
            ("/f/JetBrainsMono.ttf", "100", NEEDED), ("/f/AdwaitaMono.ttf", "100", WANTED))))
        self.assertEqual(found, "/f/AdwaitaMono.ttf")

    def test_falls_back_to_the_box_drawing_glyphs(self):
        # What fontconfig really ranked on a machine without Adwaita Mono: a proportional math font
        # first, because no font has every glyph and charset coverage outranked spacing.
        found, _, _ = self.discover(run=ran(stdout=fonts(
            ("/f/NotoSansMath.ttf", "", ASCII + " 2922"), ("/f/JetBrainsMono.ttf", "100", NEEDED))))
        self.assertEqual(found, "/f/JetBrainsMono.ttf")

    def test_dual_width_counts_as_monospace(self):
        found, _, _ = self.discover(run=ran(stdout=fonts(
            ("/f/Proportional.ttf", "", WANTED), ("/f/NerdFont.ttf", "90", WANTED))))
        self.assertEqual(found, "/f/NerdFont.ttf")

    def test_unflagged_spacing_is_the_last_resort(self):
        found, _, _ = self.discover(run=ran(stdout=fonts(
            ("/f/Plain.ttf", "100", ASCII), ("/f/NotoSansMono.ttf", "", NEEDED))))
        self.assertEqual(found, "/f/NotoSansMono.ttf")

    def test_no_font_has_the_box_drawing_glyphs(self):
        msg = self.exits(run=ran(stdout=fonts(("/f/NimbusMono.otf", "100", ASCII))))
        self.assertIn("found no monospace font", msg)

    def test_fc_match_missing(self):
        self.assertIn("no fc-match", self.exits(which=None))

    def test_fc_match_fails(self):
        msg = self.exits(run=ran(returncode=1, stderr="Fontconfig error: cannot load config"))
        self.assertIn("fc-match failed", msg)
        self.assertIn("cannot load config", msg)

    def test_fc_match_fails_silently(self):
        self.assertIn("exit status 2", self.exits(run=ran(returncode=2)))

    def test_fc_match_empty(self):
        msg = self.exits(run=ran(stdout="  \n"))
        self.assertIn("found no monospace font", msg)
        self.assertNotIn("no fc-match", msg)

    def test_fc_match_cannot_launch(self):
        self.assertIn("could not run fc-match", self.exits(run=PermissionError(13, "Permission denied")))


class ExplicitFont(unittest.TestCase):
    def test_font_flag_bypasses_discovery(self):
        with tempfile.TemporaryDirectory() as d:
            ansi = os.path.join(d, "in.ansi")
            with open(ansi, "w") as f:
                f.write("hi\n")
            argv = ["render.py", ansi, os.path.join(d, "out.png"), "--font", "/fonts/Mine.ttf"]
            with mock.patch.object(sys, "argv", argv), \
                 mock.patch.object(render, "default_font", wraps=render.default_font) as discover, \
                 mock.patch("shutil.which") as which, \
                 mock.patch("subprocess.run") as run, \
                 mock.patch.object(render, "render", return_value=(10, 20)) as draw, \
                 mock.patch("builtins.print"):
                render.main()
        self.assertEqual(draw.call_args[0][2], "/fonts/Mine.ttf")
        discover.assert_not_called()
        which.assert_not_called()
        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
