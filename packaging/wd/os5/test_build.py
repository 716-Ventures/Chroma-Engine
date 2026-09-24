"""Adversarial checks against a real OS 5 tool-produced package."""

from __future__ import annotations

import importlib.util
import io
import os
import pathlib
import struct
import subprocess
import tarfile
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parents[2]
SPEC = importlib.util.spec_from_file_location("wd_os5_build", HERE / "build.py")
assert SPEC and SPEC.loader
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)
PACKAGE = REPO / f"target/wd-os5/MyCloudEX2Ultra_chromaserver_{BUILD.VERSION}.bin"
REFERENCE = os.environ.get("CHROMA_WD_REFERENCE_PACKAGE")


class PackageInspectorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not PACKAGE.is_file():
            raise unittest.SkipTest("build the OS 5 fixture package first")
        cls.good = PACKAGE.read_bytes()

    def check_rejected(self, data: bytes):
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / "bad.bin"
            path.write_bytes(data)
            with self.assertRaises((ValueError, subprocess.CalledProcessError,
                                    tarfile.TarError, OSError)):
                BUILD.inspect_package(path, "chromaserver", BUILD.VERSION)

    def repackage(self, *, remove: str | None = None,
                  replace_signature: bool = False,
                  extra_member: str | None = None) -> bytes:
        source = io.BytesIO(self.good[204:])
        output = io.BytesIO()
        with tarfile.open(fileobj=source, mode="r:gz") as old:
            with tarfile.open(fileobj=output, mode="w:gz") as new:
                for member in old:
                    if member.name == remove:
                        continue
                    contents = old.extractfile(member) if member.isfile() else None
                    if replace_signature and member.name == "chromaserver/apkg.sign":
                        data = b"invalid signature"
                        member.size = len(data)
                        contents = io.BytesIO(data)
                    new.addfile(member, contents)
                if extra_member:
                    unsafe = tarfile.TarInfo(extra_member)
                    unsafe.size = 1
                    new.addfile(unsafe, io.BytesIO(b"x"))
        payload = output.getvalue()
        header = bytearray(self.good[:204])
        struct.pack_into("<II", header, 196, BUILD.xor_checksum(payload), len(payload))
        return bytes(header) + payload

    def test_valid_tool_output_has_full_version(self):
        self.assertEqual(BUILD.inspect_package(PACKAGE, "chromaserver", BUILD.VERSION)[1],
                         BUILD.VERSION)
        if REFERENCE:
            self.assertEqual(BUILD.inspect_package(pathlib.Path(REFERENCE), "plexmediaserver",
                                                   "1.43.4.10903")[1], "1.43.4.10903")

    def test_packaged_private_lan_setup_contract(self):
        with tarfile.open(fileobj=io.BytesIO(self.good[204:]), mode="r:gz") as archive:
            start = archive.extractfile("chromaserver/start.sh").read()
            scripts = [member for member in archive.getmembers()
                       if member.name.startswith("chromaserver/resources/admin/assets/index-")
                       and member.name.endswith(".js")]
            self.assertEqual(len(scripts), 1)
            spa = archive.extractfile(scripts[0]).read()
        self.assertIn(b"CHROMA_OWNER_SETUP_ALLOW_PRIVATE_LAN=1", start)
        self.assertNotIn(b"openssl rand -hex 32", start)
        self.assertNotIn(b"One-time setup secret", spa)
        self.assertNotIn(b"through SSH", spa)
        self.assertIn(b"Setup code", spa)
        self.assertIn(b"x-chroma-setup-secret", spa)

    def test_old_version_offset_is_rejected(self):
        data = bytearray(self.good)
        version = BUILD.VERSION.encode("ascii")
        data[72:112] = version + bytes(40 - len(version))
        self.check_rejected(bytes(data))

    def test_empty_or_mismatched_version_is_rejected(self):
        for version in (b"", b"0.1.5"):
            data = bytearray(self.good)
            data[76:112] = version + bytes(36 - len(version))
            self.check_rejected(bytes(data))

    def test_truncated_or_corrupt_payload_is_rejected(self):
        self.check_rejected(self.good[:-1])
        data = bytearray(self.good)
        data[210] ^= 1
        self.check_rejected(bytes(data))

    def test_bad_signature_or_missing_hook_is_rejected(self):
        self.check_rejected(self.repackage(replace_signature=True))
        self.check_rejected(self.repackage(remove="chromaserver/install.sh"))

    def test_archive_traversal_is_rejected(self):
        self.check_rejected(self.repackage(extra_member="chromaserver/../outside"))
        self.check_rejected(self.repackage(extra_member="chromaserver/apkg.rc"))


if __name__ == "__main__":
    unittest.main()
