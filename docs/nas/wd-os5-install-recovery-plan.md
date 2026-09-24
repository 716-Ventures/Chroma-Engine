# WD EX2 Ultra OS 5 installation recovery: execution plan for GPT-6 Sol

Prepared 2026-09-24. **Installer recovery implemented and tested on the EX2
Ultra; authenticated dashboard Off/On and broader qualification remain.** See
the [device evidence](wd-os5-install-recovery-evidence.md).

## Objective and scope

Produce a Chroma Server `.bin` that installs through the WD dashboard on the
owner's My Cloud EX2 Ultra, firmware `5.33.102`, and whose **Configure** button
opens the working Chroma administration UI. Prove installation, startup,
stop/start, and upgrade on that appliance. Build and package off-device; the
NAS must not need Rust, Node, Docker, or a compiler.

This is the immediate recovery subplan for [NAS deployment](../nas-deployment-execution-plan.md).
It does not certify playback performance, other NAS models, or production
readiness. Do not expand into codec changes or rebuild the entire media stack
unless captured runtime evidence requires it.

Execution instruction: read this whole document, then execute R0–R6 in order.
Complete all available work without repeatedly asking for permission already
given. A request to implement this plan includes the described Chroma-specific
install/update and stop/start checks. It does not include deleting other apps,
changing firmware, rebooting the entire NAS, or erasing persistent Chroma data.
If access or a builder is unavailable, finish independent work and report the
exact failed gate; do not mark the plan complete.

## Findings and confidence

### Confirmed locally on 2026-09-24: wrong version offset and inadequate validator

[`build.py`](../../packaging/wd/os5/build.py) writes the header version at decimal
offset **72** and reads it back from `[72:112]`. The owner-supplied working Plex
OS 5 package places `1.43.4.10903` at decimal offset **76**. The current Chroma
package places `0.1.4` at 72; reading at 76 therefore produces only `4`.

Running the existing `inspect_package` against the two local files returned:

```text
Plex:   ('plexmediaserver', '', 92830303)
Chroma: ('chromaserver', '0.1.4', 9719146)
```

The validator accepted an empty reference version. It shares the writer's
offset assumption and never requires the header version to equal the XML
version. Its success was insufficient evidence of WD compatibility. The
offset discrepancy is confirmed; its exact contribution to the device failure
still needs a correct-package install trace.

Reproduce with a read-only Python session from the Engine root:

```python
from pathlib import Path
import importlib.util

spec = importlib.util.spec_from_file_location("wd", "packaging/wd/os5/build.py")
wd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wd)
cases = [
    (Path("/Users/chrisjdavis/Desktop/PlexMediaServer-1.43.4.10903-e5521bd8c-MyCloudEX2Ultra_OS5.bin"), "plexmediaserver"),
    (Path("target/wd-os5/MyCloudEX2Ultra_chromaserver_0.1.4.bin"), "chromaserver"),
]
for path, app in cases:
    with path.open("rb") as stream:
        header = stream.read(204)
    print(path.name, repr(header[64:112]))
    print(wd.inspect_package(path, app))
```

Do not stop at shifting one field in the handwritten serializer. Use the OS 5
packager as the producer and establish an independent format regression test.

### Strong evidence: lifecycle argument assumptions are wrong for an OS 5 template

Current [`install.sh`](../../packaging/wd/os5/install.sh) accepts only a source
ending in `Nas_Prog/_install` and a destination ending in `Nas_Prog/chromaserver`.
The inspected OS 5 template instead treats the incoming destination as the
**parent apps directory**, appends the package name, and describes the staged
source as `_install/<app>`. Its install script moves the staged app directory
into that parent. See the pinned [helper implementation][helpers] and
[install implementation][install-template].

The October 2020 WD SDK document's illustrative paths differ from that
implementation. The supplied Plex archive uses a simple move of the two
arguments, which by itself cannot resolve their values. Therefore capture
the arguments actually passed by firmware `5.33.102`; do not blindly copy
either illustration. Our current hook exits immediately for the template's
layout. [`test_install.py`](../../packaging/wd/os5/test_install.py) constructs
our assumed layout, so its passing result does not establish the firmware
contract. This is a high-priority hypothesis, not yet a captured device cause.

### Device observations from 2026-09-23, not refreshed in this planning session

- The dashboard reported Chroma `0.1.4` and On. The app directory contained
  only `apkg.xml`; `_install` was empty, `/var/www/chromaserver` was absent,
  and no Chroma process was observed. Configure returned HTTP 404.
- A normal debug invocation opened the package and printed
  `have isntall the same module`. Other attempted reinstall flag combinations
  printed only entry/end state messages. None established a completed reinstall
  or produced the original failure trace.
- The phrase **same module** does not prove a same-version comparison. Yesterday's
  conclusion that increasing the version alone would force installation was
  unsupported. A new version is useful for artifact identity, but the correct
  update entry point must be verified separately.
- The diagnostic upload copy was removed. The local `.bin` remains. No manual
  payload installation has been demonstrated.
- ARMv7 hard-float, `/lib/ld-linux-armhf.so.3`, glibc 2.31, approximately 1 GiB
  RAM, roughly 300 MiB then available, and limited root storage were measured.
  Docker and Podman were absent. See [ARMv7 investigation](armv7-investigation.md).

## Research conclusions

WD documents model-specific `.bin` files installed through **Apps → Install an
App manually**. This is the required delivery route. [WD installation guide][manual]

The OS 5 SDK uses a Linux x86-64 packager with libxml2 and GNU tar. Run the
OS 5 tool from the package directory, whose name matches `Package:`. Have it
generate the header, XML and compatibility signature. The documented command
for this model is `mksapkg-OS5 -E -s -m MyCloudEX2Ultra`.
[WD SDK document, especially pages 4, 11–16][sdk]

An EX2 Ultra developer reported resolving an un-installable package by replacing
the OS 3 packager with `Apps/MyCloudOS5_mksapkg` from WD's GPL distribution.
That first-hand report also documents staging under `/usr/local/apps_upload`
and `upload_apkg -d -t 2 -p <basename>` for a new install. It does not establish
the update CLI for this firmware. [Developer report][debug-report]

WDCommunity's own deployment script uses **different new-install and reinstall
commands**, including `-g1` on its reinstall path. This is a useful lead, not
permission to try undocumented flags blindly. [Deployment implementation][deploy]

A maintained OS 5 build example uses Debian Bullseye with OpenSSL, libxml2,
tar and gzip. Its build helper invokes the OS 5 tool and lets it generate
metadata. Use this as a dependency baseline, pinned and verified for our
builder. [Builder image][builder-image], [build helper][build-helper]

## R0 — Preserve evidence and establish the actual baseline

1. Inspect repository instructions, branches, status and relevant diffs in
   Engine and `/Volumes/Overflow/development/GenusServer`. Preserve existing
   work. At research time Engine was at `533ad34`, one commit ahead of
   `origin/main`, with unfinished 0.1.4 changes. Do not reset or blanket-stage.
2. Save a local evidence directory under `target/wd-os5-evidence/<run-id>/`.
   Record package size/hash, source revisions, dirty patch identity, firmware,
   commands, exit codes and timestamps. Keep credentials and media titles out
   of reports. Do not publish the proprietary Plex package or its payload.
3. Refresh the NAS using the already configured SSH access: model/firmware,
   mounted data-volume path, app metadata, app/staging directory listings,
   web mapping, process executable and listener state. Read only at this step.
   Do not ask the owner to repeat dashboard version checks we can inspect.
4. Inspect the installed dashboard's app-management handler or scripts to find
   the exact command used for **update/reinstall** and its arguments. Copy a
   relevant executable locally for inspection if source is absent. Read its
   help as well. Do not dump unrelated configuration or session credentials.
   If the CLI remains unclear, use the actual dashboard update flow while
   recording diagnostics rather than cycling through flag combinations.

**Gate R0:** a current state record and a justified install/update invocation.
Keep unresolved firmware behavior explicit. No whole-NAS changes.

## R1 — Establish the off-device OS 5 packaging toolchain

1. Prefer WD's OS 5 tool from its GPL distribution. WD's [package index][gpl]
   lists firmware 5.33.102; its linked package-list download returned HTTP 403
   during research, so retrieval is not a guaranteed route.
2. The existing community-mirrored tool is a practical fallback:
   `target/wd-os5/mksapkg-OS5`, Linux x86-64 ELF, SHA-256
   `62340b1d0eb0433fffa7ceb1a5b6d4886e69b93e6e60297b2f984f5f3557e67a`.
   Pin [the upstream revision][packager], verify downloaded bytes, record that
   provenance, and check dependencies/help in the builder. Do not relabel a
   community mirror as a newly verified WD download.
3. Use an available Linux amd64 container/VM/runner off-device. First prove it
   starts and can execute the tool. On Apple Silicon, explicitly provide amd64
   execution; an ARM64 Linux shell alone is insufficient. Check disk space on
   both the VM storage and host temporary paths. Prefer external-volume storage.
   Yesterday's stalled Colima attempt and historical CI billing failure are
   reasons to check availability, not to assume either route now works.
4. Use a pinned builder with GNU tar/gzip, libxml2 and compatible OpenSSL.
   Verify the tool's Blowfish/SHA-256 command works in that environment. An
   OpenSSL 3 environment may need explicit legacy-provider support; do not
   change WD's derivation scheme, remove the signature or disable validation.
5. Make one small disposable local package with the OS 5 tool to prove it emits
   a valid archive and metadata. This need not be installed on the NAS. Preserve
   its header as an independently generated test fixture with provenance.

**Gate R1:** successful Linux tool execution and a parsable tool-generated
fixture. If a builder fails, capture its error and move to another available
route; do not return to hand-constructing the WD package.

## R2 — Replace handwritten packaging and repair the validation gates

1. Refactor `packaging/wd/os5/build.py` into staging, tool invocation and
   independent inspection. Retire manual `header_for`, `write_xml` and signature
   generation from the production path. Keep runtime binary validation.
2. Stage only Chroma payload, lifecycle scripts, web assets and required notices
   in a directory named exactly `chromaserver`. Run the verified OS 5 tool
   there with `-E -s -m MyCloudEX2Ultra`; retain complete stdout/stderr and exit
   status. Do not copy Plex's numeric identity fields as unexplained constants.
3. Take the package version from one source, initially `0.1.5` if nothing newer
   has been installed. Require agreement between output name, header, `apkg.rc`
   and generated `apkg.xml`. A version increase is not the reinstall mechanism.
4. Derive the inspector's field boundaries from the tool output. Require a
   nonempty exact version, package identity, model fields, complete payload
   length/checksum, readable archive, and valid compatibility signature.
   The local Plex reference must parse as `1.43.4.10903`, not an empty string.
5. Test with a tool-generated fixture and deliberately corrupted copies:
   the old version-offset layout, empty/mismatched version, truncated payload,
   bad checksum/signature and missing lifecycle file must fail. Reject absolute
   paths and `..` traversal, and check required script/binary executable bits.
   Do not rely only on a writer/reader round-trip sharing constants.
6. Keep the existing ARM payload fixed during packaging diagnosis. Its provenance
   records Engine `aadad79759cb763c0684dedeab4ed880cd2c3a13` and Server
   `42f0dd1e3792cee654c628d1c6133154b5430d15`. Verify those recorded hashes and
   assets before use. Clearly label this a diagnostic pilot: the known host
   SQLx macro reuse workaround remains a release reproducibility issue.

**Gate R2:** the artifact is produced by the verified OS 5 tool, and independent
tests catch the actual 0.1.4 validation defect. No NAS upload before R3 passes.

## R3 — Implement and test the lifecycle contract

1. Normalize the source/destination shapes supported by the SDK and inspected
   template, using package markers to identify the actual staged app root.
   In particular, handle `_install/chromaserver` plus the `Nas_Prog` parent.
   Preserve strict containment: never copy the entire apps directory or delete
   the shared `_install` root. Canonicalize/validate before mutation and reject
   symlink/traversal escapes. Keep database/cache/logs in `chromaserver-data`.
2. Add bounded install diagnostics **before the present path rejection**:
   script name, argument count/values, working directory, normalized paths,
   stage reached and exit status. Use a small private Chroma diagnostic file
   that survives staging cleanup; do not log the full environment. Every hook
   should report failures so metadata-only registration cannot look like success.
3. Copy/move into the resolved app root without an extra nested `chromaserver`
   directory. Support an existing metadata-only destination. Require the server,
   engine, SPA and hooks to exist with correct permissions before install succeeds.
4. Test start/stop/clean without arguments as well as supported argument forms.
   Ensure background startup survives the hook's shell exiting, preserves stdout
   and stderr, prevents duplicate starts and checks the executable before killing
   a PID. Keep load errors visible instead of discarding Engine stderr.
5. Use bounded HTTP readiness in startup; `kill -0` after two seconds is only
   process existence. Trace the actual Server `/ready` contract and record the
   response. Report a failed startup with its log location when the deadline ends.
6. Verify the Configure web mapping and its PHP syntax against this firmware.
   Start with the supplied package's empty `AddonUsedPort` plus `index.php`
   pattern. Publish only the redirect/status assets if practical, with readable
   permissions for WD's web server. The redirect should target the NAS host on
   Chroma's actual port; check readiness rather than accepting any TCP listener.
7. Expand the install test matrix using Linux BusyBox-compatible shell behavior:
   nested staged app plus parent destination, the documented alternate layout,
   metadata-only destination, missing payload, path escape rejection, no-argument
   lifecycle calls, idempotent start/init, failed readiness and preserved data.
   Derive expectations from the independent contract, not the implementation.

**Gate R3:** local lifecycle tests pass and the new package contains those exact
hooks. Initial device traces must verify their actual arguments before the
firmware contract is marked confirmed.

## R4 — Run one instrumented installation and classify the first failure

1. Preserve the current Chroma metadata and any existing Chroma data before
   update. Confirm the upload directory resolves to the data volume and that
   space is sufficient for the package, extracted payload and backup.
2. Upload the exact R2/R3 artifact once, verify its remote hash, then invoke the
   update/install path established in R0. Capture full installer output and its
   exit code locally. Do not mask it with `tee` pipeline status. Capture small
   state files and hook diagnostics before WD removes staging.
3. If the registered metadata-only app prevents installation, use WD's verified
   Chroma removal/reinstall flow after preserving data. If that flow is itself
   broken, first specify the exact app-only recovery and backup; do not issue
   broad delete/rescan commands borrowed from OS 3 examples.
4. Check actual installed files against the artifact manifest, correct directory
   layout, permissions, web mapping, hook stages, process executable and HTTP.

| First failing boundary | Required next action |
| --- | --- |
| Existing-module/early exit before extraction | Verify update-handler invocation and state; a version-only rebuild is not evidence of a fix. |
| Header/archive/signature rejected | Compare with the tool-generated fixture and inspect the exact error; do not change URL or server code. |
| Extraction succeeded but install hook rejected arguments | Use captured arguments to correct normalization and add that exact case to tests. |
| Payload installed but init/web mapping failed | Inspect WD's actual web root/alias and permissions; test PHP separately from server startup. |
| Engine or Server cannot load | Capture loader/library/symbol/signal output; only then adjust the off-device ARM build. |
| Process starts but `/ready` fails | Inspect Chroma's log, data permissions, bind address/port and migration state. |
| Direct Chroma UI works but Configure fails | Trace the dashboard URL, redirect response and destination UI separately. |

Every subsequent artifact must identify the captured failure it changes, the
regression test added and its new hash. Do not submit another speculative
package to the owner. Remove only our exact staged upload when evidence has
been saved; preserve the local artifact.

**Gate R4:** the WD-managed install actually placed the payload and executed
its hooks. A dashboard version label or CLI exit code alone cannot pass.

## R5 — Device acceptance

Record HTTP status/body, timings, process identity and artifact hash for each:

1. Chroma `/ready` responds successfully on port 32410, and `/` serves the SPA
   with its referenced assets. Confirm the Server's current readiness semantics.
2. The exact URL opened by **Configure** returns the expected redirect/status
   and opens the functioning administration UI from a LAN browser. Test from
   the client as well as localhost. Include the dashboard's actual scheme/port.
3. WD Off stops only Chroma; WD On restores readiness. Repeating On does not
   create a second process. Record startup duration and memory/swap observations.
4. Perform one WD-managed upgrade to the next package revision, proving the
   pre-existing app case. Verify persistent-data continuity, hook completion,
   new payload hashes, readiness and Configure again. Preserve a rollback copy.
5. Confirm cleanup/removal preserves `chromaserver-data` in local lifecycle tests;
   record actual appliance uninstall/reinstall separately if performed. Full NAS
   reboot persistence is a separate pending check until the owner schedules it.

**Gate R5:** all of items 1–4 pass on the EX2 Ultra. If browser authentication
requires the owner, finish machine-verifiable checks first and request only
that final UI interaction. Do not represent playback or reboot as tested.

## R6 — Handoff and durable documentation

Deliver the tested `.bin`, SHA-256, source revisions and concise install steps.
Save a sanitized evidence report containing the observed installer/hook
contract, failure and fix, actual HTTP results, timing and remaining limitations.
Update [packaging README](../../packaging/wd/os5/README.md),
[ARMv7 investigation](armv7-investigation.md) and
[execution ledger](execution-ledger.md): their claims that 0.1.4 has not been
tried are stale. Replace the old compatibility claim with the specific proven
checks. Preserve historical attempts as failures, not successful qualification.

Commit/push the scoped implementation and documentation once requested
execution is complete, preserving unrelated edits. Inspect any already-unpushed
commit before including it. Verify the remote ref; local tests do not establish
remote CI success. Do not mark this recovery complete until R5 passes.

## Execution ledger to maintain

| Gate | Status at handoff | Evidence required |
| --- | --- | --- |
| Research | complete | Sources below; local header/validator reproduction above |
| R0 current baseline/update invocation | complete | Fresh device snapshot; verified WD CGI reinstall invocation with basename and `-g 1` |
| R1 Linux OS 5 packager | complete | Pinned mirrored tool, amd64 builder, fixture |
| R2 packaging/validation | complete | Tool-produced 0.1.6 package; independent reference and corruption checks |
| R3 lifecycle | complete | Local contract tests; device traces confirmed argument shape and exact hooks |
| R4 instrumented install | complete | WD-managed 0.1.5 install, hook trace, installed payload and readiness |
| R5 device acceptance | partial | Configure, readiness, upgrade and hook stop/start passed; authenticated dashboard Off/On pending |
| R6 delivery | in progress | 0.1.6 artifact and sanitized report prepared; scoped commit/push and dashboard gate remain |

## Source register

Online sources inspected 2026-09-24. Primary implementations are pinned where
possible. Community code is evidence of its own behavior, not an unconditional
specification for the owner's firmware. Some WD URLs return access errors;
those are documented above rather than treated as successful retrievals.

- [WD manual app installation][manual]: supported delivery workflow.
- [WD OS 5 open-source package index][gpl]: includes firmware 5.33.102.
- [WD October 2020 OS 5 App Structure PDF, community-hosted copy][sdk]: build
  prerequisites, lifecycle overview; cross-check illustrative paths on-device.
- [EX2 Ultra developer's installation report][debug-report]: OS 5 packager
  correction and new-install debugging command.
- [WDCommunity OS 5 packager][packager]: pinned mirror for tool provenance.
- [WDCommunity deployment script][deploy]: separate install/reinstall branches.
- [OS 5 helper paths][helpers] and [install hook][install-template]: independent
  evidence for staged app directory and parent-destination handling.
- [OS 5 build helper][build-helper] and [builder image][builder-image]: off-device
  tool invocation and Linux dependency baseline.

[manual]: https://support-en.wd.com/app/answers/detailweb/a_id/29960/~/steps-to-download-and-install-third-party-apps-manually-on-my-cloud-os-5
[gpl]: https://support-en.wd.com/app/answers/detailweb/a_id/30295
[sdk]: https://github.com/paul-norman/WD-NAS-App-Builder/blob/f5c2e8288e824264f06ac05a3b13d5de9751e2e3/guides/MyCloud_OS5_AppStructure_Oct2020.pdf
[debug-report]: https://community.wd.com/t/how-to-debug-failing-installation-of-own-app/261466
[packager]: https://github.com/WDCommunity/wdpksrc/blob/f72c74ad046ce44fe0a86452e14f5c0eff3226b8/mksapkg-OS5
[deploy]: https://github.com/WDCommunity/wdpksrc/blob/f72c74ad046ce44fe0a86452e14f5c0eff3226b8/build_and_install.sh
[helpers]: https://github.com/paul-norman/WD-NAS-App-Builder/blob/f5c2e8288e824264f06ac05a3b13d5de9751e2e3/apps/helpers.sh
[install-template]: https://github.com/paul-norman/WD-NAS-App-Builder/blob/f5c2e8288e824264f06ac05a3b13d5de9751e2e3/apps/template/install.sh
[build-helper]: https://github.com/paul-norman/WD-NAS-App-Builder/blob/f5c2e8288e824264f06ac05a3b13d5de9751e2e3/apps/build_helpers.sh
[builder-image]: https://github.com/paul-norman/WD-NAS-App-Builder/blob/f5c2e8288e824264f06ac05a3b13d5de9751e2e3/docker/build.Dockerfile
