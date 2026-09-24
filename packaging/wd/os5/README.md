# My Cloud EX2 Ultra OS 5 app package

This is a model-specific, off-device-built WD OS 5 package for Chroma Server.
It was installed and upgraded on a My Cloud EX2 Ultra running firmware
`5.33.102` on 2026-09-24. The tested package is
`target/wd-os5/MyCloudEX2Ultra_chromaserver_0.1.6.bin` (SHA-256
`a963edce2f919759325f96cfff68dc06fbe2257dafd949cd5dfb2df8bbc9814a`).
It contains the ARMv7 diagnostic pilot Engine and Server binaries and the
administration SPA. No compiler, Rust, Node, or container runtime is needed on
the NAS. The pilot's SQLx build workaround remains a release-reproducibility
issue; this is not general NAS or playback qualification.

The package must be built on Linux amd64 using the pinned OS 5 `mksapkg-OS5`
tool. `Dockerfile` supplies the Linux dependencies and OpenSSL legacy provider
required by WD's compatibility signature. The mirrored tool's SHA-256 is
`62340b1d0eb0433fffa7ceb1a5b6d4886e69b93e6e60297b2f984f5f3557e67a`;
verify its provenance before distributing a release. `build.py` verifies the
pilot binary hashes, runs the WD packager, and independently inspects the
resulting header, metadata, signature, payload and file modes. Version comes
from `apkg.rc`.

From this repository, with the pilot bundle and tool at their default paths:

```sh
docker build --platform linux/amd64 -t chroma-wd-os5-builder:bookworm \
  -f packaging/wd/os5/Dockerfile packaging/wd/os5
python3 packaging/wd/os5/build.py
CHROMA_WD_DOCKER_CONTEXT=default python3 -m unittest \
  packaging/wd/os5/test_build.py packaging/wd/os5/test_install.py -v
```

Use `--docker-context NAME` for a non-default Docker context. On Apple
Silicon, verify that the context can execute `linux/amd64`; an ARM64 Linux
shell alone cannot run the packager. `build.py` refuses to overwrite an
existing output. Set `CHROMA_WD_REFERENCE_PACKAGE` to a locally held known-good
OS 5 package to run the optional reference-parser assertion; never commit
that package. Keep the prior `.bin` as a rollback artifact.

Install or update through the WD dashboard's **Apps → Install an App manually**
flow. On the tested firmware, WD's reinstall handler stages
`_install/chromaserver`, invokes `install.sh` with that path and the `Nas_Prog`
parent, then runs `init.sh` and `start.sh`. It leaves app files in
`Nas_Prog/chromaserver` and Chroma's private data in the sibling
`Nas_Prog/chromaserver-data`. Chroma's removal hook preserves that data
directory. Do not manually delete it during an update. The verified internal
WD CLI reinstall requires the upload **basename** plus `-r`, `-f 1`, and
`-g 1`; a path or omitted `-g` did not install anything despite exit code 0.
Use the dashboard for normal operation.

After installation, `http://NAS-LAN-ADDRESS:32410/ready` must return 200 and
`ready: true`. The dashboard's Configure link should open
`http://NAS-LAN-ADDRESS/chromaserver/index.php`, which redirects to the
administration UI on port 32410. On a fresh server, the UI asks the owner to
set a password. Chroma's hook log is
`Nas_Prog/chromaserver-data/install-hooks.log`; startup and Engine load logs
are in the same private data directory. The short `startup-status.txt` in the
app directory can be shown by Configure if startup fails. Keep port 32410 on
the trusted LAN; do not forward it to the internet.

The 0.1.4 dashboard registration was metadata-only: it did not install the
payload. Versions 0.1.5 and 0.1.6 installed and started through WD's manager;
0.1.6 fixed Configure's PHP readiness check for this firmware, where
`allow_url_fopen` is disabled. The 0.1.5→0.1.6 upgrade preserved the server
identity and returned `/ready` in about one second. Chroma-specific hook
stop/start and repeated start passed. Authenticated WD dashboard Off/On,
media playback, whole-NAS reboot persistence, and other NAS models remain
unverified. See `docs/nas/wd-os5-install-recovery-evidence.md`.
