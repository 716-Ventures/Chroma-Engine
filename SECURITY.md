# Security Policy

## Supported Versions

Chroma Engine is an Apache-2.0 project maintained by 716 Ventures. It is currently
pre-1.0. Security fixes are applied to `main` and to actively maintained release
branches; older tags and arbitrary historical commits are not automatically
maintained.

Use the [GitHub releases page](https://github.com/716-Ventures/Chroma-Engine/releases)
to identify published releases and their security or upgrade notes. A draft
release, prerelease, successful local build, or capability probe is not a claim
of production security qualification. Applications embedding the engine must
update their own pinned dependency or bundled executable to receive fixes.

## Reporting a Vulnerability

Report suspected vulnerabilities privately. Do not publish exploit details,
crafted media, sensitive logs, or proof-of-concept code in public issues,
discussions, or pull requests before coordinated disclosure.

Check the repository's [security advisories page](https://github.com/716-Ventures/Chroma-Engine/security/advisories).
If **Report a vulnerability** is available, use that private reporting channel.
As of this policy update, private vulnerability reporting is not enabled and
there is no published security email address.

Until a private channel is available, open an issue titled **Private security
contact request**, with no vulnerability details or attachments, asking a
maintainer to arrange a private reporting channel. Do not send the reproducer
until that channel has been established. This contact request is not the
vulnerability report itself.

Once a private channel is established, include:

- The engine version and exact commit SHA, enabled Cargo features, and whether
  it runs in-process or as a CLI worker.
- OS/version, CPU architecture, and relevant GPU, driver, or NAS/filesystem details.
- The affected API or CLI command, reproduction steps, expected behavior, and
  observed impact.
- The smallest input or generator you can legally share, identifying the
  container, tracks/codecs, or subtitle format involved.
- Sanitized errors, crash traces, and relevant resource policy, timeout, memory,
  or concurrency settings. For resource exhaustion, include input size and
  measured resource use when available.

Remove tokens, credentials, personal information, and unrelated private paths.
Do not upload complete copyrighted movies as reproducers. Prefer a minimized
sample you have permission to share or a synthetic input generator.

## Scope and Safe Testing

Security-relevant issues include memory-safety failures; crafted-input crashes,
hangs, or resource-limit bypasses; unintended filesystem reads/writes or path
traversal; unsafe artifact publication; and worker-isolation or cancellation
failures. These can occur in probing, container/codec parsing, subtitles,
remuxing, segmentation, native backends, or output handling.

An unsupported format being rejected within configured limits is not by itself
a vulnerability. If malformed media can bypass limits or disrupt the host,
report that behavior privately even if the format is unsupported.

Test only systems and media you own or have permission to assess. Use an isolated
environment with bounded CPU, memory, and disk use. Do not test against other
people's servers, access their data, or run denial-of-service tests on shared
infrastructure without explicit permission.

## Response Targets

Critical parser, path traversal, arbitrary write, or crash-on-playback issues should receive initial triage within 2 business days. High-severity denial-of-service and malformed-media issues should receive initial triage within 5 business days.

These are best-effort triage targets after maintainers receive a private report,
not guaranteed remediation deadlines or a service-level agreement. Maintainers
will coordinate investigation, mitigations, fixes, and disclosure timing with the
reporter. Please allow time for affected host applications to update their engine
dependency before publishing exploit details. Reporter credit can be coordinated
with the reporter's consent.

## Deployment Security Boundary

The engine does not provide authentication, HTTP authorization, or media-library
access control. Hosts must authorize input paths, isolate cache directories,
bound queues and disk usage, and supervise abandoned work. Do not run media
workers with unnecessary privileges or expose arbitrary filesystem paths to clients.

Share resource admission across engine work in a host process. In-process limits
are not an OS memory ceiling, and cooperative cancellation cannot interrupt every
blocked filesystem or native codec/driver call. Use supervised workers and external
CPU/memory limits when processing untrusted media requires stronger isolation.
See [resource policy and worker integration](docs/resource-policy.md).

Reports involving a dependency or native backend are welcome when they affect
Chroma Engine. Maintainers may coordinate with the relevant upstream project;
Apache-2.0 licensing does not make third-party code, GPU drivers, or system SDKs
part of Chroma's own implementation or guarantee their security.

## Release Hardening

Release artifacts must be built from a clean commit, include checksums, and pass `cargo test --locked --all-targets --all-features`, `cargo clippy --locked --all-targets --all-features -- -D warnings`, `cargo deny check`, rustdoc warnings, and fuzz-target smoke checks before distribution.

The [release policy](docs/release-policy.md) also requires dependency attribution
checks, bundled native-library notices, and auditable dependency metadata.
Checksums detect differences from the expected artifact; they are not by
themselves proof of publisher identity or a substitute for security testing.
