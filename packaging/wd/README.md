# WD EX2 Ultra ARMv7 pilot bundle

This is an off-device-built **load-test bundle**, not a My Cloud OS 5 `.bin`
app. Do not use the dashboard's manual app-install flow with this archive. It
has no vendor lifecycle integration, startup script, updater, or rollback.

The bundle contains `chroma-server`, `resources/bin/chroma-engine`, the admin
web assets, Engine license and third-party notices, SHA-256 sums, and source
revision identifiers. The binaries target ARMv7 EABI5 hard-float Linux and a
glibc 2.31 baseline. Building and inspecting ELF files on a Mac does not prove
they load or perform adequately on the appliance.

Before a device test, agree on a writable data-volume staging directory with
enough space, a backup/cleanup plan, and a brief test window. The NAS root
filesystem has too little free space for this bundle. Do not use a production
library or expose a test service to the internet. Start by verifying both
executables' loader behavior on the NAS, then `/ready` with a fresh test data
directory, and only then a small authorized media fixture. Watch RSS, swap,
CPU, and startup time. Stop on OOM, sustained swapping, or load errors.

The native My Cloud OS 5 `.bin` app is built separately with
`packaging/wd/os5/build.py`. This bundle remains a load-test artifact; neither
artifact has yet run on the NAS.
