"""Exercise the actual Linux package/install scripts against isolated homes."""
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]


class LinuxPackageTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="awili packaging ")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.repo = self.root / "source"
        (self.repo / "scripts").mkdir(parents=True)
        shutil.copy(REPO / "scripts/package-linux.sh", self.repo / "scripts")
        shutil.copytree(REPO / "packaging/linux", self.repo / "packaging/linux")
        (self.repo / "Cargo.toml").write_text('[workspace.package]\nversion = "0.3.0-rc.2"\n')
        (self.repo / "assets/brand/app-icons/linux/hicolor").mkdir(parents=True)
        self.bins = self.root / "binary inputs"
        self.bins.mkdir()
        self.app = self.bins / "awili-app"
        shutil.copy('/usr/bin/true', self.app)
        self.helper = self.bins / "awili-connectors-mcp"
        self.out = self.root / "output"
        self.env = dict(os.environ, HOME=str(self.root / "home"),
                        XDG_BIN_HOME=str(self.root / "install bins"),
                        XDG_DATA_HOME=str(self.root / "install data"),
                        XDG_CONFIG_HOME=str(self.root / "settings"))

    def make_private(self):
        p = self.repo / "crates/awili-connectors-mcp/Cargo.toml"
        p.parent.mkdir(parents=True)
        p.write_text('[package]\nname="awili-connectors-mcp"\n')

    def package(self, extra=(), success=True):
        r = subprocess.run(['sh', str(self.repo / 'scripts/package-linux.sh'),
                            str(self.app), str(self.out), *map(str, extra)],
                           env=self.env, capture_output=True, text=True)
        self.assertEqual(r.returncode == 0, success, r.stdout + r.stderr)
        if not success:
            return
        archive = next(self.out.glob('*.tar.gz'))
        dest = self.root / "unpacked"
        with tarfile.open(archive) as t:
            t.extractall(dest, filter='data')
        return next(dest.iterdir())

    def run_installer(self, stage, script):
        subprocess.run(['sh', str(stage / script)], env=self.env,
                       check=True, capture_output=True)

    def test_private_companion_install_uninstall_preserves_data(self):
        self.make_private()
        shutil.copy('/usr/bin/true', self.helper)
        stage = self.package()
        self.assertTrue((stage / self.helper.name).is_file())
        marker = Path(self.env['XDG_DATA_HOME']) / 'awili' / 'keep.db'
        marker.parent.mkdir(parents=True)
        marker.write_text('keep')
        self.run_installer(stage, 'install.sh')
        for n in ['awili-app', 'awili-connectors-mcp']:
            self.assertTrue(os.access(Path(self.env['XDG_BIN_HOME']) / n, os.X_OK))
        self.run_installer(stage, 'uninstall.sh')
        for n in ['awili-app', 'awili-connectors-mcp']:
            self.assertFalse((Path(self.env['XDG_BIN_HOME']) / n).exists())
        self.assertEqual(marker.read_text(), 'keep')

    def test_private_missing_companion_refuses_archive(self):
        self.make_private()
        self.package(success=False)
        self.assertFalse(self.out.exists())

    def test_public_does_not_accidentally_bundle_adjacent_private_helper(self):
        shutil.copy('/usr/bin/true', self.helper)
        stage = self.package()
        self.assertFalse((stage / self.helper.name).exists())
        self.run_installer(stage, 'install.sh')
        self.assertTrue((Path(self.env['XDG_BIN_HOME']) / 'awili-app').exists())
        self.assertFalse((Path(self.env['XDG_BIN_HOME']) / self.helper.name).exists())

    def test_explicit_helper_is_bundled(self):
        p = self.root / 'explicit helper'
        shutil.copy('/usr/bin/true', p)
        stage = self.package([p])
        self.assertTrue((stage / self.helper.name).exists())

    def test_explicit_missing_helper_refuses_archive(self):
        self.package([self.root / 'missing'], success=False)


if __name__ == '__main__':
    unittest.main()
