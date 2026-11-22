fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rerun-if-changed=Info.plist");
        println!("cargo:rerun-if-changed=src/local/speech_permissions.h");
        println!("cargo:rerun-if-changed=src/login_item.m");
        cc::Build::new()
            .file("src/login_item.m")
            .flag("-fobjc-arc")
            .flag("-mmacosx-version-min=12.3")
            .compile("clipy_login_item");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=ServiceManagement");
        println!("cargo:rustc-link-lib=framework=CoreServices");
        println!("cargo:rerun-if-changed=src/clipboard_mac.m");
        cc::Build::new()
            .file("src/clipboard_mac.m")
            .flag("-fobjc-arc")
            .flag("-mmacosx-version-min=12.3")
            .compile("clipy_clipboard");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=Carbon");
        println!("cargo:rerun-if-changed=src/camera_effects.m");
        cc::Build::new()
            .file("src/camera_effects.m")
            .flag("-fobjc-arc")
            .flag("-mmacosx-version-min=12.3")
            .compile("clipy_camera_effects");
        // Vision is person segmentation for camera backgrounds (camera_effects.m).
        println!("cargo:rustc-link-lib=framework=Vision");
        println!("cargo:rustc-link-lib=framework=CoreImage");
        println!("cargo:rerun-if-changed=src/local/apple.m");
        cc::Build::new()
            .file("src/local/apple.m")
            .flag("-fobjc-arc")
            .flag("-Wno-deprecated-declarations")
            .flag("-mmacosx-version-min=12.3")
            .compile("clipy_local");
        // Live transcription (apple_stream.m) is a separate translation unit so
        // the file-based ops in apple.m keep their own small surface.
        println!("cargo:rerun-if-changed=src/local/apple_stream.m");
        cc::Build::new()
            .file("src/local/apple_stream.m")
            .flag("-fobjc-arc")
            .flag("-Wno-deprecated-declarations")
            .flag("-mmacosx-version-min=12.3")
            .compile("clipy_local_stream");
        // SpeechAnalyzer (macOS 26) is Swift-only, so its live engine is a
        // small Swift static library behind the same C surface.
        build_speech_analyzer();
        for framework in [
            "AVFoundation",
            // AVAudioFormat / AVAudioPCMBuffer for the live transcription feed.
            "AVFAudio",
            "Speech",
            "Security",
            "CoreMedia",
            "ImageIO",
            "CoreGraphics",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
    // tauri-build only watches resource files that already exist, so a model
    // added to this folder was never copied next to the dev executable.
    #[cfg(target_os = "windows")]
    println!("cargo:rerun-if-changed=segmentation");
    // The local edition (no `cloud` feature) has no updater plugin, so it
    // validates its own capability set, which leaves out `updater:default`.
    // Cargo tells a build script the package's features through the
    // environment, not through `cfg`.
    if std::env::var_os("CARGO_FEATURE_CLOUD").is_some() {
        tauri_build::build()
    } else {
        println!("cargo:rerun-if-changed=capabilities-local");
        tauri_build::try_build(
            tauri_build::Attributes::new()
                .capabilities_path_pattern("./capabilities-local/**/*"),
        )
        .expect("failed to run tauri-build for the local edition");
    }
}

/// Compiles `src/local/speech_analyzer.swift` into a static library and adds
/// the Swift runtime search paths its autolink entries need. The runtime ships
/// in macOS (`/usr/lib/swift`, macOS 10.14.4+, Concurrency 12+), so nothing is
/// bundled; the SDK's `.tbd` stubs satisfy the linker and the toolchain's
/// `macosx` folder holds the small static back-deployment shims.
#[cfg(target_os = "macos")]
fn build_speech_analyzer() {
    use std::path::PathBuf;
    use std::process::Command;

    const SOURCE: &str = "src/local/speech_analyzer.swift";
    println!("cargo:rerun-if-changed={SOURCE}");

    let xcrun = |args: &[&str]| -> String {
        let output = Command::new("xcrun")
            .args(args)
            .output()
            .unwrap_or_else(|error| panic!("xcrun {args:?} could not run: {error}"));
        assert!(
            output.status.success(),
            "xcrun {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };

    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => panic!("unsupported macOS target architecture for Swift: {other:?}"),
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let library = out.join("libclipy_speech_analyzer.a");
    let module_cache = out.join("swift-module-cache");
    let target = format!("{arch}-apple-macos12.3");

    let output = Command::new("xcrun")
        .args(["--sdk", "macosx", "swiftc"])
        .args(["-parse-as-library", "-emit-library", "-static"])
        .args(["-swift-version", "5", "-O"])
        .args(["-module-name", "ClipySpeechAnalyzer"])
        .args(["-target", &target])
        .arg("-module-cache-path")
        .arg(&module_cache)
        .arg("-o")
        .arg(&library)
        .arg(SOURCE)
        .output()
        .expect("xcrun swiftc could not run; install Xcode or the Command Line Tools");
    assert!(
        output.status.success(),
        "swiftc failed for {SOURCE}:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let sdk = PathBuf::from(xcrun(&["--sdk", "macosx", "--show-sdk-path"]));
    let swiftc = PathBuf::from(xcrun(&["--sdk", "macosx", "--find", "swiftc"]));
    // .../usr/bin/swiftc -> .../usr/lib/swift/macosx
    let toolchain_lib = swiftc
        .parent()
        .and_then(|bin| bin.parent())
        .map(|usr| usr.join("lib/swift/macosx"))
        .expect("swiftc is not inside a toolchain usr/bin folder");

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=clipy_speech_analyzer");
    println!(
        "cargo:rustc-link-search=native={}",
        sdk.join("usr/lib/swift").display()
    );
    println!("cargo:rustc-link-search=native={}", toolchain_lib.display());
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
