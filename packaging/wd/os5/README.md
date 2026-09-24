# My Cloud EX2 Ultra OS 5 app package

This is a model-specific, off-device-built WD OS 5 package for Chroma Server.
It was installed and upgraded on a My Cloud EX2 Ultra running firmware
`5.33.102` on 2026-09-24. The last device-tested package is
`target/wd-os5/MyCloudEX2Ultra_chromaserver_0.1.8.bin` (SHA-256
`8bbbbe0c74d9f19761abfb5aeb96ed9db1aeac67d178f0ff4b376e568c35a6bd`).
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

From this repository, with the OS 5 tool at its default path. Rebuild both
the ARMv7 Server binary and current GenusServer admin SPA before assembling a
version-specific pilot bundle. Pass that bundle with `--pilot-dir` and the
SPA's `dist` directory with `--admin-dir`:

```sh
docker build --platform linux/amd64 -t chroma-wd-os5-builder:bookworm \
  -f packaging/wd/os5/Dockerfile packaging/wd/os5
python3 packaging/wd/os5/build.py \
  --pilot-dir /absolute/path/to/wd-ex2-ultra-armv7-pilot-0.1.9 \
  --admin-dir /absolute/path/to/GenusServer/apps/admin-spa/dist
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

Version 0.1.8 changed first-run setup on this WD package: from a direct
private-LAN connection, enter and confirm an owner password of at least 12
characters. SSH and a separate setup code are not required. The package opts
in to this narrow API allowance. Public or proxy-trusted connections still
require a separately configured setup secret; this package does not create
one. Keep the NAS on a trusted LAN during initial setup because another LAN
client could otherwise claim an unconfigured server first. The rejected 0.1.7
candidate required SSH for the setup secret and should not be installed.
The owner successfully installed 0.1.8 and completed owner setup on the NAS.

Version 0.1.9 is a superseded candidate for the first large TV scan: the 0.1.8
scan reached only about 170 of 1,507 files during the observed test window.
It probed files serially, and metadata matching waited for the scan to finish;
the exact source of the observed latency has not been measured. In the
small-NAS profile, 0.1.9 imports files first and leaves technical probing for
on-demand playback. This avoids serial probe delays during indexing, but a
file's first playback can still pay the probe cost. TV counts are computed
from imported rows while the scan is running; the sidebar counts distinct
series, not episode files. Scan progress and failures are persisted so Activity
can show them after a page refresh. The 0.1.9 scan and subsequent metadata
matching were not verified on the appliance.

Complete technical analysis is required for unplayed files too. Follow the
[staged scanner execution plan](../../../docs/nas/staged-scanner-execution-plan.md)
to restore mandatory probes, share the full probe result with playback, and
measure actual NAS performance. The replacement is planned, not implemented;
0.1.9 must not be presented as the completed scanning fix.

The 0.1.4 dashboard registration was metadata-only: it did not install the
payload. Versions 0.1.5 and 0.1.6 installed and started through WD's manager;
0.1.6 fixed Configure's PHP readiness check for this firmware, where
`allow_url_fopen` is disabled. The 0.1.5→0.1.6 upgrade preserved the server
identity and returned `/ready` in about one second. Chroma-specific hook
stop/start and repeated start passed. Authenticated WD dashboard Off/On,
media playback, whole-NAS reboot persistence, and other NAS models remain
unverified. See `docs/nas/wd-os5-install-recovery-evidence.md`.
