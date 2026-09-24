# Off-device WD EX2 Ultra pilot build

This recipe targets the measured OS 5 firmware 5.33.102 ABI: ARMv7 EABI5
hard-float, `/lib/ld-linux-armhf.so.3`, glibc 2.31. It builds a test bundle;
`packaging/wd/os5/build.py` then wraps it in a dashboard-installable `.bin`.
Run on a development Mac with Rust 1.97.1,
Zig 0.16, `cargo-zigbuild` 0.23.4, Meson, Ninja, pkg-config, and LLVM 21.
Use an absolute `SERVER_REPO` path to the GenusServer checkout.

```sh
rustup target add --toolchain 1.97.1 armv7-unknown-linux-gnueabihf
BUILD_DIR=$(mktemp -d /tmp/chroma-armv7-cross.XXXXXX)
git clone https://code.videolan.org/videolan/dav1d.git "$BUILD_DIR/dav1d"
git -C "$BUILD_DIR/dav1d" checkout b546257f770768b2c88258c533da38b91a06f737
meson setup "$BUILD_DIR/dav1d-build" "$BUILD_DIR/dav1d" \
  --cross-file packaging/cross/wd-armv7-zig.ini --buildtype=release \
  --default-library=static -Denable_asm=false -Denable_tools=false \
  -Denable_tests=false --prefix="$BUILD_DIR/prefix" --libdir=lib
ninja -C "$BUILD_DIR/dav1d-build" -j4
ninja -C "$BUILD_DIR/dav1d-build" install
```

From the Engine checkout, with `SERVER_REPO` set:

```sh
export CC_armv7_unknown_linux_gnueabihf="$PWD/packaging/cross/wd-armv7-cc"
export CXX_armv7_unknown_linux_gnueabihf="$PWD/packaging/cross/wd-armv7-cxx"
export AR_armv7_unknown_linux_gnueabihf=/opt/homebrew/opt/llvm@21/bin/llvm-ar
export CARGO_BUILD_JOBS=4
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export PKG_CONFIG_PATH="$BUILD_DIR/prefix/lib/pkgconfig"
export PKG_CONFIG_ALLOW_CROSS=1
export SYSTEM_DEPS_DAV1D_LINK=static
export CARGO_TARGET_DIR="$BUILD_DIR/engine-target"
cargo +1.97.1 zigbuild --locked --release \
  --target armv7-unknown-linux-gnueabihf.2.31 --bin chroma-engine
ENGINE_BINARY="$CARGO_TARGET_DIR/armv7-unknown-linux-gnueabihf/release/chroma-engine"
```

The Server must use the same Zig target and C/C++/archive wrappers:

```sh
cd "$SERVER_REPO"
export CARGO_TARGET_DIR="$BUILD_DIR/server-target"
cargo +1.97.1 zigbuild --locked --release \
  --target armv7-unknown-linux-gnueabihf.2.31 -p chroma-server
SERVER_BINARY="$CARGO_TARGET_DIR/armv7-unknown-linux-gnueabihf/release/chroma-server"
npm run build -w @chroma-server/admin-spa
cd -
packaging/wd/build-pilot-bundle.sh "$ENGINE_BINARY" "$SERVER_BINARY" "$SERVER_REPO" \
  "$PWD/target/wd-ex2-ultra-armv7-pilot-0.1.12"
python3 packaging/wd/os5/build.py \
  --pilot-dir "$PWD/target/wd-ex2-ultra-armv7-pilot-0.1.12" \
  --admin-dir "$SERVER_REPO/apps/admin-spa/dist"
```

The first Server cross-build on the development Mac failed to load its newly
created host `sqlx-macros` dylib (`mis-aligned LINKEDIT string pool`). The
pilot binary was linked after reusing a previously valid host macro dylib
from the same Server revision. Until a clean build passes, this recipe is
documented as an attempted build path, **not** a verified reproducible release
procedure. The `.bin` format has been checked against a known-good package,
but never treat that as physical device qualification. Validate both
executables and the app lifecycle on the NAS before wider distribution.
