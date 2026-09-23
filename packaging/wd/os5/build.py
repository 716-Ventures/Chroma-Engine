#!/usr/bin/env python3
"""Build a My Cloud OS 5 EX2 Ultra app from the off-device ARMv7 pilot."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import io
import pathlib
import shutil
import struct
import subprocess
import tarfile
import tempfile
import xml.etree.ElementTree as ET


APP = "chromaserver"
VERSION = "0.1.2"
MAGIC = b"GrandTeZ"
HEADER_SIZE = 204
MODEL_FIELDS = (2, 0, 20, 1, 9, 1)
# My Cloud's public manual-app format uses this compatibility passphrase.
SIGN_KEY = "Lidho.mdk3K3h"
SCRIPT_NAMES = (
    "install.sh", "init.sh", "preinst.sh", "start.sh", "stop.sh",
    "clean.sh", "remove.sh",
)


def xor_checksum(payload: bytes) -> int:
    # OS 5 ignores the last one to three bytes, rather than zero-padding them.
    result = 0
    for offset in range(0, len(payload) - len(payload) % 4, 4):
        result ^= struct.unpack_from("<I", payload, offset)[0]
    return result


def header_for(payload: bytes) -> bytes:
    header = bytearray(HEADER_SIZE)
    header[:8] = MAGIC
    header[8:8 + len(APP)] = APP.encode("ascii")
    header[72:72 + len(VERSION)] = VERSION.encode("ascii")
    struct.pack_into("<6I", header, 112, *MODEL_FIELDS)
    struct.pack_into("<II", header, 196, xor_checksum(payload), len(payload))
    return bytes(header)


def inspect_package(path: pathlib.Path, expected_app: str):
    with path.open("rb") as stream:
        header = stream.read(HEADER_SIZE)
        payload = stream.read()
    if len(header) != HEADER_SIZE or header[:8] != MAGIC:
        raise ValueError("not a My Cloud EX2 Ultra OS 5 package")
    name = header[8:72].split(b"\0", 1)[0].decode("ascii")
    version = header[72:112].split(b"\0", 1)[0].decode("ascii")
    model_fields = struct.unpack_from("<6I", header, 112)
    checksum, length = struct.unpack_from("<II", header, 196)
    if name != expected_app or model_fields != MODEL_FIELDS:
        raise ValueError("package identity or EX2 Ultra model fields do not match")
    if length != len(payload) or checksum != xor_checksum(payload):
        raise ValueError("package length or checksum does not match payload")
    with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as archive:
        names = set(archive.getnames())
        for member in archive.getmembers():
            if member.name != expected_app and not member.name.startswith(expected_app + "/"):
                raise ValueError("archive member escapes package root")
            if member.issym() or member.islnk():
                raise ValueError("archive contains a link")
        required = {f"{expected_app}/{name}" for name in SCRIPT_NAMES}
        required.update({f"{expected_app}/apkg.rc", f"{expected_app}/apkg.xml",
                         f"{expected_app}/apkg.sign"})
        if not required.issubset(names):
            raise ValueError("package lacks required app metadata or scripts")
        sign_member = archive.extractfile(f"{expected_app}/apkg.sign")
        if sign_member is None:
            raise ValueError("missing package signature")
        signature = sign_member.read()
        if expected_app == APP:
            rc_member = archive.extractfile(f"{APP}/apkg.rc")
            xml_member = archive.extractfile(f"{APP}/apkg.xml")
            if rc_member is None or xml_member is None:
                raise ValueError("missing Chroma package metadata")
            rc_fields = dict(line.split(":", 1) for line in
                             rc_member.read().decode("utf-8").splitlines() if ":" in line)
            xml_item = ET.fromstring(xml_member.read()).find("./apkg/item")
            if (rc_fields.get("Version", "").strip() != VERSION
                    or rc_fields.get("AddonUsedPort", "").strip()
                    or xml_item is None
                    or xml_item.findtext("url_port") not in (None, "")):
                raise ValueError("Configure metadata must use the dashboard PHP redirect")
    decrypted = subprocess.run(
        ["openssl", "bf-cbc", "-d", "-md", "sha256", "-k", SIGN_KEY],
        input=signature, capture_output=True, check=True,
    ).stdout
    if decrypted != (expected_app + "\n").encode("ascii"):
        raise ValueError("package signature does not match app name")
    return name, version, len(payload)


def validate_arm_binary(path: pathlib.Path):
    data = path.read_bytes()
    if len(data) < 52 or data[:6] != b"\x7fELF\x01\x01":
        raise ValueError(f"not an ELF32 little-endian binary: {path}")
    if struct.unpack_from("<H", data, 18)[0] != 40:
        raise ValueError(f"not ARM: {path}")
    if struct.unpack_from("<I", data, 36)[0] & 0x400 != 0x400:
        raise ValueError(f"not ARM hard-float: {path}")
    if b"/lib/ld-linux-armhf.so.3" not in data:
        raise ValueError(f"unexpected Linux loader: {path}")


def write_xml(path: pathlib.Path):
    root = ET.Element("config")
    apkg = ET.SubElement(root, "apkg")
    item = ET.SubElement(apkg, "item")
    fields = {
        "procudt_id": "0", "custom_id": "20", "model_id": "1",
        "app_id": "9", "user_control": "1", "center_type": "0",
        "individual_flag": "1", "name": APP, "show": "Chroma Server",
        "enable": "0", "version": VERSION,
        "date": datetime.date.today().strftime("%Y%m%d"), "inst_date": "",
        "path": "", "ps_name": "", "url": "index.php",
        "url_port": "", "apkg_version": "2",
        "packager": "716 Ventures", "email": "",
        "homepage": "https://github.com/716-Ventures/GenusServer",
        "inst_depend": "", "inst_conflict": "", "start_depend": "",
        "start_conflict": "", "description": "Chroma Server for personal media libraries.",
        "icon": "", "MinFWVer": "5.33.102", "MaxFWVer": "", "Hidden": "",
    }
    for key, value in fields.items():
        ET.SubElement(item, key).text = value
    ET.ElementTree(root).write(path, encoding="utf-8", xml_declaration=True)


def normalized_tar_info(info: tarfile.TarInfo):
    info.uid = 0
    info.gid = 0
    info.uname = "root"
    info.gname = "root"
    return info


def build(pilot_dir: pathlib.Path, output: pathlib.Path, reference: pathlib.Path | None):
    source_dir = pathlib.Path(__file__).resolve().parent
    if reference:
        inspect_package(reference, "plexmediaserver")
    validate_arm_binary(pilot_dir / "chroma-server")
    validate_arm_binary(pilot_dir / "resources/bin/chroma-engine")
    sums = (pilot_dir / "SHA256SUMS").read_text(encoding="ascii").splitlines()
    expected = {}
    for line in sums:
        digest, relative = line.split(maxsplit=1)
        expected[relative] = digest
    for relative in ("chroma-server", "resources/bin/chroma-engine"):
        actual = hashlib.sha256((pilot_dir / relative).read_bytes()).hexdigest()
        if expected.get(relative) != actual:
            raise ValueError(f"pilot checksum mismatch: {relative}")
    if not (pilot_dir / "resources/admin/index.html").is_file():
        raise ValueError("pilot lacks admin SPA")
    if output.exists():
        raise FileExistsError(f"refusing to overwrite {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="chroma-wd-os5-") as temp:
        app_dir = pathlib.Path(temp) / APP
        app_dir.mkdir()
        shutil.copy2(pilot_dir / "chroma-server", app_dir / "chroma-server")
        shutil.copytree(pilot_dir / "resources", app_dir / "resources")
        for name in ("SHA256SUMS", "BUILD-PROVENANCE.txt"):
            shutil.copy2(pilot_dir / name, app_dir / name)
        for name in SCRIPT_NAMES:
            shutil.copy2(source_dir / name, app_dir / name)
            (app_dir / name).chmod(0o755)
        shutil.copy2(source_dir / "index.php", app_dir / "index.php")
        shutil.copy2(source_dir / "apkg.rc", app_dir / "apkg.rc")
        write_xml(app_dir / "apkg.xml")
        sign = subprocess.run(
            ["openssl", "bf-cbc", "-md", "sha256", "-k", SIGN_KEY],
            input=(APP + "\n").encode("ascii"), capture_output=True, check=True,
        ).stdout
        (app_dir / "apkg.sign").write_bytes(sign)

        compressed = io.BytesIO()
        with tarfile.open(fileobj=compressed, mode="w:gz", format=tarfile.GNU_FORMAT) as archive:
            archive.add(app_dir, arcname=APP, filter=normalized_tar_info)
        payload = compressed.getvalue()
        output.write_bytes(header_for(payload) + payload)
    inspect_package(output, APP)
    return output


def main():
    repo = pathlib.Path(__file__).resolve().parents[3]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pilot-dir", type=pathlib.Path,
                        default=repo / "target/wd-ex2-ultra-armv7-pilot")
    parser.add_argument("--output", type=pathlib.Path,
                        default=repo / "target/wd-os5/MyCloudEX2Ultra_chromaserver_0.1.2.bin")
    parser.add_argument("--reference", type=pathlib.Path,
                        help="optional known-good EX2 Ultra OS 5 .bin for format cross-check")
    args = parser.parse_args()
    print(build(args.pilot_dir, args.output, args.reference))


if __name__ == "__main__":
    main()
