# My Cloud EX2 Ultra OS 5 app package

`build.py` wraps the off-device ARMv7 Engine and Server binaries, admin SPA,
license notices, and OS 5 lifecycle hooks in a dashboard-installable `.bin`.
It targets the measured EX2 Ultra firmware `5.33.102`; it does not contain
Plex code or files. The package header, model identifiers, payload length,
XOR checksum, and Blowfish app-name compatibility signature were checked
against an owner-supplied working EX2 Ultra OS 5 package. This verifies the
format, **not** successful installation or Chroma runtime behavior on the NAS.

From the Engine repository after building the pilot bundle:

```sh
python3 packaging/wd/os5/build.py
```

The output is `target/wd-os5/MyCloudEX2Ultra_chromaserver_0.1.2.bin`.
Use the WD dashboard's **Apps → Install an App manually** flow. Do not
rename the pilot `.tar.gz`; it is not a WD app package. The app installs under
the NAS's `Nas_Prog/chromaserver` directory and keeps database, cache, temporary
files, and logs in the sibling `chromaserver-data` directory on the data
volume. Logs rotate on restart if larger than 5 MiB. The package's removal
hook deletes only the app directory and preserves that data directory; actual
dashboard uninstall behavior still needs device verification.
The server uses the small-NAS resource profile and listens on port 32410 on
the LAN. Do not forward its ports to the internet.

After installation, open `http://NAS-LAN-ADDRESS:32410/ready` and then
`http://NAS-LAN-ADDRESS:32410/`. Start with a small authorized library. If
the server does not open, Configure shows a short startup status without
revealing logs or private paths. Capture that status. The `0.1.2` `.bin`
has not yet been uploaded to or run on this NAS.

The package scripts are intentionally model/path-specific. They do not modify
firmware or other app folders, and they preserve `chromaserver-data` during
upgrade/uninstall. Runtime, memory, playback, update, and rollback remain
physical-device qualification gates.
