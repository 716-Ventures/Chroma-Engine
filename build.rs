use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SDKROOT");
    println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    // The AudioToolbox Swift bridge is a static dependency. Its build-script
    // link arguments do not propagate to our binaries, examples, and tests.
    let sdk = std::env::var("SDKROOT").unwrap_or_else(|_| {
        let output = Command::new("xcrun")
            .args(["--sdk", "macosx", "--show-sdk-path"])
            .output()
            .expect("xcrun is required to locate the macOS SDK");
        assert!(output.status.success(), "could not locate the macOS SDK");
        String::from_utf8(output.stdout)
            .expect("macOS SDK path must be UTF-8")
            .trim()
            .to_owned()
    });
    println!("cargo:rustc-link-search=native={sdk}/usr/lib/swift");
    for library in ["swiftCore", "swiftDarwin", "swiftCoreAudio"] {
        println!("cargo:rustc-link-lib=dylib={library}");
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
