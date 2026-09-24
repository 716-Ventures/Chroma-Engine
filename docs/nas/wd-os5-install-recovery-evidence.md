# WD EX2 Ultra OS 5 installation recovery evidence

Recorded 2026-09-24. This report deliberately omits LAN addresses, login
details, private library content, and the proprietary reference package.

## Scope and artifact

Physical device: My Cloud EX2 Ultra, ARMv7, firmware 5.33.102, approximately
1 GiB RAM, glibc 2.31, no container runtime. The data volume had more than
2 TiB free. At the start, WD showed Chroma 0.1.4 On but the app directory
contained only `apkg.xml`; no payload, web link, or process existed.

The final package is `MyCloudEX2Ultra_chromaserver_0.1.6.bin`, 9,743,256
bytes, SHA-256
`a963edce2f919759325f96cfff68dc06fbe2257dafd949cd5dfb2df8bbc9814a`.
The rollback package is 0.1.5, SHA-256
`58064c9c5ff88c2c8a566de46b0d82fbc5f4108cf0ab639e5ee5f0b4ecaee82d`.
Both use the same diagnostic ARM pilot: Engine
`aadad79759cb763c0684dedeab4ed880cd2c3a13`, Server
`42f0dd1e3792cee654c628d1c6133154b5430d15`.
Recovery source began from Engine `533ad34844a19794a6087fd0aa7c53ae98bbfd25`
and Server checkout `95601e82d29fd54bec7a0a17bbb4ada4c076ce43`.
The package was produced by Linux amd64 `mksapkg-OS5` version 2.0, mirrored
at the pinned WDCommunity revision in the recovery plan, SHA-256
`62340b1d0eb0433fffa7ceb1a5b6d4886e69b93e6e60297b2f984f5f3557e67a`.
The independently inspected owner-supplied reference parsed with full version
`1.43.4.10903`; no reference payload is distributed.

## Failure boundaries and fixes

1. The old handwritten serializer put the header version at byte 72; OS 5's
   packager puts it at byte 76. The old validator accepted an empty reference
   version. Production now uses the OS 5 tool and adversarial inspection tests.
2. WD's `upload_apkg` help and dashboard CGI show separate install/reinstall
   paths. The tested reinstall required `-r` with the upload **basename**,
   `-f 1`, and `-g 1`. An omitted `-g` returned exit 0 without extraction; an
   absolute `-r` path was incorrectly prefixed by `/usr/local/apps_upload`.
   Those failed probes did not modify the installed Chroma payload.
3. The successful WD trace called `install.sh` with
   `_install/chromaserver` and the `Nas_Prog` parent. The hook copied the payload
   into the app root, then `init.sh` and `start.sh` ran. The private hook log
   recorded `payload-validated`, `payload-installed`, `exit=0`, `init
   completed`, and `start ready` in one second. That is stronger evidence than
   the dashboard's version label or the CLI exit code alone.
4. The 0.1.5 server loaded Engine and returned HTTP 200 with JSON `ready:true`.
   Configure still returned 503 because firmware PHP has `allow_url_fopen`
   disabled, although PHP cURL can fetch `/ready`. Version 0.1.6 changed the
   readiness probe to PHP cURL. Its PHP syntax passed on the NAS; Configure
   returned HTTP 302 to the LAN-host Chroma URL on port 32410.
5. A Chroma-specific web directory needed mode 755 under `umask 077`. The
   hook now sets it explicitly. The firmware's `/apps/...` URL returned 403
   even for WD's other apps; the dashboard's actual Chroma URL is
   `/chromaserver/index.php`, served by the app-specific alias.

## Device checks

The WD-managed 0.1.5→0.1.6 upgrade ran stop, clean, preinstall, remove,
install, init and start hooks. Installed `apkg.xml` reports 0.1.6 and the
payload contains Server, Engine, SPA and all hooks. The SHA-256 of
`server-id.txt` was unchanged across upgrade:
`5e52bb87d6d7415d570a4eed284e88e570c9dd946397031c5a744ca9b356a959`.
The data directory and diagnostic records were preserved.

After upgrade, loopback and LAN `/ready` returned 200 with
`chromaEngine`, `database`, `dataDirWritable` and `ready` all true. The LAN
Configure URL returned 302 to the same NAS host on port 32410. The root SPA
returned 200 and a referenced JavaScript asset returned 200. A browser
followed Configure to the Chroma setup screen. Creating the owner's password
is intentionally left to the owner.

Chroma-specific `stop.sh` made port 32410 refuse connections; `start.sh`
restored `/ready`; repeated `start.sh` returned success with one Chroma Server
process. The last observed process was approximately 34 MiB resident; system
memory available was approximately 289 MiB, with about 114 MiB swap used.
This is a snapshot, not sustained performance qualification. Local tests:
16/16 passed, including tool-format corruption checks, install path safety,
bounded failed startup, data preservation, and Linux web-link idempotence.

An exploratory invocation of `apkg -h` unexpectedly launched a device-wide
app manager pass; it was stopped. It triggered Chroma stop/start/clean hooks
and also printed activity from two other installed WD apps. Their state was
not changed intentionally, but a full post-pass audit of those apps was not
performed. Do not use `apkg -h` as a help command on this firmware.

## Remaining acceptance

The first browser setup attempt after 0.1.6 returned HTTP 403: "Remote owner
setup requires the one-time setup secret." The network-enabled API required
`CHROMA_OWNER_SETUP_SECRET`, while the 0.1.6 hook did not set it and the SPA
had no field for it. Version 0.1.7 was built as an off-device candidate with
a private setup secret and matching form, but rejected because normal setup
would require SSH. Version 0.1.8 instead opts this WD package in to
password-only owner setup from a direct private-LAN peer, with public and
proxy-trusted requests still requiring a configured secret. The ARM Server
binary and SPA must both be rebuilt for this change. Package installation
and owner setup on the NAS remain unverified for 0.1.8.

Authenticated dashboard Off/On remains unverified: an unauthenticated local
call to WD's `cgi_apps_set` returned HTTP 403. The Chroma hook-level stop/start
check is not a substitute for the dashboard toggle. A whole-NAS reboot,
authorized-library scan and playback, sustained resource measurements,
large-file behavior, other NAS models, and clean Server build reproducibility
also remain. Do not claim the broader NAS plan or playback qualification is
complete on the strength of this installer recovery.
