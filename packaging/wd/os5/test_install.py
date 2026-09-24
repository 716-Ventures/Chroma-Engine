"""Contract tests for WD's staged-app and apps-parent install arguments."""

from __future__ import annotations

import pathlib
import os
import shutil
import subprocess
import tempfile
import unittest

HERE = pathlib.Path(__file__).parent
REPO = HERE.parents[2]
HOOKS = ("install.sh", "init.sh", "preinst.sh", "start.sh", "stop.sh",
         "clean.sh", "remove.sh")


def make_stage(path: pathlib.Path) -> None:
    path.mkdir(parents=True)
    for hook in HOOKS:
        shutil.copy2(HERE / hook, path / hook)
        (path / hook).chmod(0o755)
    for name in ("chroma-server", "resources/bin/chroma-engine"):
        binary = path / name
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_text("#!/bin/sh\nexit 1\n", encoding="utf-8")
        binary.chmod(0o755)
    admin = path / "resources/admin/index.html"
    admin.parent.mkdir(parents=True)
    admin.write_text("admin", encoding="utf-8")
    for name in ("apkg.rc", "apkg.xml", "index.php"):
        (path / name).write_text(name, encoding="utf-8")


class InstallContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.programs = pathlib.Path(self.temporary.name) / "Nas_Prog"
        self.programs.mkdir()

    def run_install(self, source: pathlib.Path, destination: pathlib.Path,
                    success: bool = True) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(["sh", str(HERE / "install.sh"), str(source),
                                 str(destination)], text=True, capture_output=True)
        self.assertEqual(result.returncode == 0, success, result.stderr)
        return result

    def test_nested_stage_and_parent_destination(self):
        staged = self.programs / "_install/chromaserver"
        make_stage(staged)
        self.run_install(staged, self.programs)
        installed = self.programs / "chromaserver"
        self.assertEqual((installed / "index.php").read_text(), "index.php")
        self.assertTrue((installed / "resources/bin/chroma-engine").is_file())
        self.assertIn("payload-installed", (self.programs / "chromaserver-data/install-hooks.log").read_text())

    def test_existing_metadata_only_directory_is_populated(self):
        staged = self.programs / "_install/chromaserver"
        make_stage(staged)
        installed = self.programs / "chromaserver"
        installed.mkdir()
        (installed / "apkg.xml").write_text("old", encoding="utf-8")
        data = self.programs / "chromaserver-data"
        data.mkdir()
        (data / "keep.db").write_text("persistent", encoding="utf-8")
        self.run_install(staged, installed)
        self.assertEqual((installed / "apkg.xml").read_text(), "apkg.xml")
        self.assertEqual((data / "keep.db").read_text(), "persistent")

    def test_sdk_flat_stage_is_supported_without_copying_shared_staging_root(self):
        stage = self.programs / "_install"
        make_stage(stage)
        self.run_install(stage, self.programs / "chromaserver")
        self.assertTrue((stage / "install.sh").is_file())
        self.assertTrue((self.programs / "chromaserver/chroma-server").is_file())

    def test_missing_payload_fails_before_creating_app(self):
        staged = self.programs / "_install/chromaserver"
        make_stage(staged)
        (staged / "chroma-server").unlink()
        self.run_install(staged, self.programs, success=False)
        self.assertFalse((self.programs / "chromaserver").exists())

    def test_symlinked_stage_and_destination_are_rejected(self):
        staged = self.programs / "_install/chromaserver"
        make_stage(staged)
        alias = self.programs / "_install/alias"
        alias.symlink_to(staged)
        self.run_install(alias, self.programs, success=False)
        (self.programs / "chromaserver").symlink_to(self.programs)
        self.run_install(staged, self.programs, success=False)

    def test_path_outside_staging_root_is_rejected(self):
        staged = self.programs / "elsewhere/chromaserver"
        make_stage(staged)
        self.run_install(staged, self.programs, success=False)

    def test_no_argument_start_and_stop_use_script_directory(self):
        installed = self.programs / "chromaserver"
        make_stage(installed)
        result = subprocess.run(["sh", str(installed / "start.sh")],
                                cwd=self.temporary.name, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((installed / "startup-status.txt").read_text(),
                         "engine-load-failed\n")
        secret = self.programs / "chromaserver-data/owner-setup-secret"
        self.assertFalse(secret.exists())
        subprocess.run(["sh", str(installed / "stop.sh")],
                       cwd=self.temporary.name, check=True)

    def test_prior_candidate_bootstrap_secret_is_removed_on_upgrade_start(self):
        installed = self.programs / "chromaserver"
        make_stage(installed)
        data = self.programs / "chromaserver-data"
        data.mkdir()
        (data / "owner-setup-secret").write_text("a" * 64 + "\n", encoding="ascii")
        subprocess.run(["sh", str(installed / "start.sh")], capture_output=True)
        self.assertFalse((data / "owner-setup-secret").exists())

    def test_failed_readiness_is_bounded_and_removes_pid(self):
        installed = self.programs / "chromaserver"
        make_stage(installed)
        engine = installed / "resources/bin/chroma-engine"
        engine.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        server = installed / "chroma-server"
        server.write_text(
            '#!/bin/sh\nprintf "%s\\n" "$CHROMA_OWNER_SETUP_ALLOW_PRIVATE_LAN" '
            '> "$CHROMA_DATA_DIR/lan-setup-flag"\nsleep 10\n', encoding="utf-8")
        fake_bin = pathlib.Path(self.temporary.name) / "bin"
        fake_bin.mkdir()
        curl = fake_bin / "curl"
        curl.write_text("#!/bin/sh\nexit 7\n", encoding="utf-8")
        curl.chmod(0o755)
        environment = os.environ | {"PATH": f"{fake_bin}:{os.environ['PATH']}",
                                    "CHROMA_READY_ATTEMPTS": "1"}
        result = subprocess.run(["sh", str(server.parent / "start.sh")],
                                env=environment, capture_output=True, timeout=8)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((installed / "startup-status.txt").read_text(),
                         "server-not-ready\n")
        self.assertFalse((self.programs / "chromaserver-data/chroma-server.pid").exists())
        self.assertEqual((self.programs / "chromaserver-data/lan-setup-flag").read_text(),
                         "1\n")

    def test_removal_preserves_data(self):
        installed = self.programs / "chromaserver"
        make_stage(installed)
        data = self.programs / "chromaserver-data"
        data.mkdir()
        (data / "keep.db").write_text("persistent", encoding="utf-8")
        subprocess.run(["sh", str(installed / "remove.sh"), str(installed)], check=True)
        self.assertFalse(installed.exists())
        self.assertEqual((data / "keep.db").read_text(), "persistent")

    @unittest.skipUnless(os.environ.get("CHROMA_WD_DOCKER_CONTEXT"),
                         "set CHROMA_WD_DOCKER_CONTEXT for Linux web-root test")
    def test_init_is_idempotent_and_web_directory_is_readable(self):
        script = (
            "set -e; mkdir -p /tmp/Nas_Prog/chromaserver /var/www/apps; "
            "cp /work/packaging/wd/os5/index.php /tmp/Nas_Prog/chromaserver/index.php; "
            "sh /work/packaging/wd/os5/init.sh /tmp/Nas_Prog/chromaserver; "
            "sh /work/packaging/wd/os5/init.sh /tmp/Nas_Prog/chromaserver; "
            "test \"$(stat -c %a /var/www/apps/chromaserver)\" = 755; "
            "test \"$(readlink /var/www/apps/chromaserver/index.php)\" = "
            "/tmp/Nas_Prog/chromaserver/index.php"
        )
        subprocess.run(["docker", "--context", os.environ["CHROMA_WD_DOCKER_CONTEXT"],
                        "run", "--rm", "--platform", "linux/amd64", "-v",
                        f"{REPO}:/work:ro", "chroma-wd-os5-builder:bookworm",
                        "sh", "-ec", script], check=True, capture_output=True)


if __name__ == "__main__":
    unittest.main()
