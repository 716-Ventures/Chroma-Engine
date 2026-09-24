#!/usr/bin/env python3
"""Build the EX2 Ultra app with the OS 5 mksapkg tool, then inspect it."""

from __future__ import annotations

import argparse
import hashlib
import io
import pathlib
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import xml.etree.ElementTree as ET

APP = "chromaserver"
PACKAGE_RC = pathlib.Path(__file__).resolve().parent / "apkg.rc"
VERSION = next(line.split(":", 1)[1].strip() for line in PACKAGE_RC.read_text().splitlines()
               if line.startswith("Version:"))
HEADER_SIZE = 204
MODEL_FIELDS = (2, 0, 20, 1, 9, 1)  # Observed OS 5 mksapkg output for EX2 Ultra.
PACKAGER_SHA256 = "62340b1d0eb0433fffa7ceb1a5b6d4886e69b93e6e60297b2f984f5f3557e67a"
SIGN_KEY = "Lidho.mdk3K3h"
HOOKS = ("install.sh", "init.sh", "preinst.sh", "start.sh", "stop.sh",
         "clean.sh", "remove.sh")


def xor_checksum(payload: bytes) -> int:
    result = 0
    for offset in range(0, len(payload) - len(payload) % 4, 4):
        result ^= struct.unpack_from("<I", payload, offset)[0]
    return result


def inspect_package(path: pathlib.Path, expected_app: str,
                    expected_version: str | None = None) -> tuple[str, str, int]:
    with path.open("rb") as stream:
        header, payload = stream.read(HEADER_SIZE), stream.read()
    if len(header) != HEADER_SIZE or header[:8] != b"GrandTeZ":
        raise ValueError("invalid OS 5 EX2 Ultra package header")
    # Independent of the producer: OS 5 mksapkg puts version at byte 76.
    name = header[8:76].split(b"\0", 1)[0].decode("ascii")
    version = header[76:112].split(b"\0", 1)[0].decode("ascii")
    if name != expected_app or not version or (expected_version and version != expected_version):
        raise ValueError("header name/version mismatch")
    if struct.unpack_from("<6I", header, 112) != MODEL_FIELDS:
        raise ValueError("EX2 Ultra model fields mismatch")
    checksum, length = struct.unpack_from("<II", header, 196)
    if length != len(payload) or checksum != xor_checksum(payload):
        raise ValueError("package payload length/checksum mismatch")
    with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as archive:
        listing = archive.getmembers()
        members = {member.name: member for member in listing}
        if len(members) != len(listing):
            raise ValueError("duplicate archive path")
        for member in members.values():
            parts = pathlib.PurePosixPath(member.name)
            if (parts.is_absolute() or ".." in parts.parts or parts.parts[0] != expected_app
                    or not (member.isfile() or member.isdir())):
                raise ValueError("unsafe package archive member")
        required = {f"{expected_app}/{entry}" for entry in HOOKS}
        required.update({f"{expected_app}/apkg.rc", f"{expected_app}/apkg.xml",
                         f"{expected_app}/apkg.sign"})
        if expected_app == APP:
            required.update({f"{APP}/chroma-server", f"{APP}/resources/bin/chroma-engine",
                             f"{APP}/resources/admin/index.html", f"{APP}/index.php"})
        if not required.issubset(members):
            raise ValueError("package is missing required files")
        executable = HOOKS + (("chroma-server", "resources/bin/chroma-engine")
                              if expected_app == APP else ())
        for entry in executable:
            member = members[f"{expected_app}/{entry}"]
            if not member.isfile() or not member.mode & 0o111:
                raise ValueError(f"required executable has wrong mode: {entry}")
        rc = archive.extractfile(members[f"{expected_app}/apkg.rc"])
        xml = archive.extractfile(members[f"{expected_app}/apkg.xml"])
        sign = archive.extractfile(members[f"{expected_app}/apkg.sign"])
        if rc is None or xml is None or sign is None:
            raise ValueError("metadata/signature is not a regular file")
        fields = dict(line.split(":", 1) for line in rc.read().decode().splitlines()
                      if ":" in line)
        item = ET.fromstring(xml.read()).find("./apkg/item")
        if (fields.get("Package", "").strip() != expected_app
                or fields.get("Version", "").strip() != version or item is None
                or item.findtext("name") != expected_app or item.findtext("version") != version):
            raise ValueError("header and metadata disagree")
        if (expected_app == APP and
                (fields.get("AddonUsedPort", "").strip()
                 or item.findtext("url_port") not in (None, "")
                 or item.findtext("url") != "index.php")):
            raise ValueError("Configure metadata mismatch")
        signature = sign.read()
    commands = (["openssl", "bf-cbc", "-d", "-md", "sha256", "-k", SIGN_KEY],
                ["openssl", "bf-cbc", "-d", "-md", "sha256", "-k", SIGN_KEY,
                 "-provider", "default", "-provider", "legacy"])
    result = None
    for command in commands:
        result = subprocess.run(command, input=signature, capture_output=True)
        if result.returncode == 0:
            break
    if result is None or result.returncode:
        raise ValueError("signature cannot be decrypted")
    if result.stdout != (expected_app + "\n").encode("ascii"):
        raise ValueError("signature does not match package name")
    return name, version, length


def validate_arm_binary(path: pathlib.Path) -> None:
    data = path.read_bytes()
    if (len(data) < 52 or data[:6] != b"\x7fELF\x01\x01"
            or struct.unpack_from("<H", data, 18)[0] != 40
            or struct.unpack_from("<I", data, 36)[0] & 0x400 != 0x400
            or b"/lib/ld-linux-armhf.so.3" not in data):
        raise ValueError(f"not an ARMv7 hard-float binary for the NAS: {path}")


def build(pilot_dir: pathlib.Path, output: pathlib.Path, packager: pathlib.Path,
          docker_context: str | None, reference: pathlib.Path | None,
          admin_dir: pathlib.Path | None = None) -> pathlib.Path:
    source = pathlib.Path(__file__).resolve().parent
    if reference:
        inspect_package(reference, "plexmediaserver", "1.43.4.10903")
    if hashlib.sha256(packager.read_bytes()).hexdigest() != PACKAGER_SHA256:
        raise ValueError("OS 5 packager hash mismatch")
    expected = {relative: digest for digest, relative in
                (line.split(maxsplit=1) for line in
                 (pilot_dir / "SHA256SUMS").read_text(encoding="ascii").splitlines())}
    for relative in ("chroma-server", "resources/bin/chroma-engine"):
        binary = pilot_dir / relative
        validate_arm_binary(binary)
        if hashlib.sha256(binary.read_bytes()).hexdigest() != expected.get(relative):
            raise ValueError(f"pilot checksum mismatch: {relative}")
    if not (pilot_dir / "resources/admin/index.html").is_file():
        raise ValueError("pilot lacks admin SPA")
    if admin_dir and not (admin_dir / "index.html").is_file():
        raise ValueError("admin override lacks index.html")
    admin_source = admin_dir or pilot_dir / "resources/admin"
    if not any(b"x-chroma-setup-secret" in asset.read_bytes()
               for asset in (admin_source / "assets").glob("index-*.js")):
        raise ValueError("admin SPA lacks setup-code fallback support; pass --admin-dir")
    if output.exists():
        raise FileExistsError(f"refusing to overwrite {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="chroma-wd-os5-", dir=output.parent) as temp:
        stage = pathlib.Path(temp)
        app_dir = stage / APP
        app_dir.mkdir()
        tool = stage / "mksapkg-OS5"
        shutil.copy2(packager, tool)
        tool.chmod(0o755)
        shutil.copy2(pilot_dir / "chroma-server", app_dir)
        shutil.copytree(pilot_dir / "resources", app_dir / "resources")
        if admin_dir:
            shutil.rmtree(app_dir / "resources/admin")
            shutil.copytree(admin_dir, app_dir / "resources/admin")
        for entry in ("SHA256SUMS", "BUILD-PROVENANCE.txt"):
            shutil.copy2(pilot_dir / entry, app_dir)
        for entry in HOOKS:
            shutil.copy2(source / entry, app_dir)
            (app_dir / entry).chmod(0o755)
        shutil.copy2(source / "index.php", app_dir)
        shutil.copy2(source / "apkg.rc", app_dir)
        command = ["docker"]
        if docker_context:
            command.extend(["--context", docker_context])
        command.extend(["run", "--rm", "--platform", "linux/amd64", "-v",
                        f"{stage}:/work:rw", "-w", "/work/chromaserver",
                        "chroma-wd-os5-builder:bookworm", "/work/mksapkg-OS5",
                        "-E", "-s", "-m", "MyCloudEX2Ultra"])
        result = subprocess.run(command, text=True, capture_output=True, check=False)
        print(result.stdout, end="")
        print(result.stderr, end="", file=sys.stderr)
        if result.returncode:
            raise RuntimeError(f"OS 5 packager failed (exit {result.returncode})")
        candidates = list(stage.glob(f"MyCloudEX2Ultra_{APP}_{VERSION}.bin(*)"))
        if len(candidates) != 1:
            raise ValueError("packager did not create exactly one expected artifact")
        inspect_package(candidates[0], APP, VERSION)
        shutil.move(candidates[0], output)
    inspect_package(output, APP, VERSION)
    return output


def main() -> None:
    repo = pathlib.Path(__file__).resolve().parents[3]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pilot-dir", type=pathlib.Path,
                        default=repo / "target/wd-ex2-ultra-armv7-pilot")
    parser.add_argument("--output", type=pathlib.Path,
                        default=repo / f"target/wd-os5/MyCloudEX2Ultra_{APP}_{VERSION}.bin")
    parser.add_argument("--packager", type=pathlib.Path,
                        default=repo / "target/wd-os5/mksapkg-OS5")
    parser.add_argument("--docker-context")
    parser.add_argument("--reference", type=pathlib.Path)
    parser.add_argument("--admin-dir", type=pathlib.Path,
                        help="rebuilt Server administration SPA directory")
    args = parser.parse_args()
    print(build(args.pilot_dir, args.output, args.packager, args.docker_context,
                args.reference, args.admin_dir))


if __name__ == "__main__":
    main()
