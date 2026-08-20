use std::env;
use std::path::PathBuf;
use std::process::Command;

fn run(mut command: Command, description: &str) {
    let status = command.status().unwrap_or_else(|error| {
        panic!("failed to start {description}: {error}");
    });
    assert!(status.success(), "{description} failed with {status}");
}

fn pkg_config(option: &str, packages: &[&str]) -> Vec<String> {
    let output = Command::new("pkg-config")
        .arg(option)
        .args(packages)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "failed to start pkg-config for {}: {error}",
                packages.join(", ")
            )
        });
    assert!(
        output.status.success(),
        "pkg-config could not find {}: {}",
        packages.join(", "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("pkg-config output is UTF-8")
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn pkg_config_path(variable: &str, package: &str) -> PathBuf {
    let option = format!("--variable={variable}");
    let values = pkg_config(&option, &[package]);
    assert_eq!(
        values.len(),
        1,
        "pkg-config returned an invalid {variable} for {package}"
    );
    PathBuf::from(&values[0])
}

fn main() {
    for variable in [
        "FFMPEG_DIR",
        "WAYLAND_PROTOCOLS_DIR",
        "PKG_CONFIG_PATH",
        "PKG_CONFIG_LIBDIR",
        "PKG_CONFIG_SYSROOT_DIR",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let ffmpeg_dir = env::var_os("FFMPEG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            manifest_dir
                .parent()
                .expect("the project directory has a parent")
                .join("ffmpeg")
        });
    let wayland_protocols_dir = env::var_os("WAYLAND_PROTOCOLS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| pkg_config_path("pkgdatadir", "wayland-protocols"));

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let bindings = out_dir.join("bindings.rs");
    let renderer_object = out_dir.join("video_renderer.o");
    let input_object = out_dir.join("wayland_input.o");
    let protocol_object = out_dir.join("xdg_shell_protocol.o");
    let renderer_archive = out_dir.join("libvideo_renderer.a");
    let protocol_header = out_dir.join("xdg-shell-client-protocol.h");
    let protocol_source = out_dir.join("xdg-shell-protocol.c");

    println!("cargo:rerun-if-changed=native/bindings.h");
    println!("cargo:rerun-if-changed=native/video_renderer.c");
    println!("cargo:rerun-if-changed=native/video_renderer.h");
    println!("cargo:rerun-if-changed=native/wayland_input.c");
    println!("cargo:rerun-if-changed=native/wayland_input.h");

    let protocol_xml = wayland_protocols_dir.join("stable/xdg-shell/xdg-shell.xml");
    println!("cargo:rerun-if-changed={}", protocol_xml.display());
    let mut scanner_header = Command::new("wayland-scanner");
    scanner_header
        .arg("client-header")
        .arg(&protocol_xml)
        .arg(&protocol_header);
    run(scanner_header, "Wayland xdg-shell header generation");
    let mut scanner_source = Command::new("wayland-scanner");
    scanner_source
        .arg("private-code")
        .arg(&protocol_xml)
        .arg(&protocol_source);
    run(scanner_source, "Wayland xdg-shell code generation");

    let mut bindgen = Command::new("bindgen");
    bindgen
        .arg("native/bindings.h")
        .args([
            "--allowlist-function",
            "(SDL|av|avcodec|avformat|avsubtitle|swr|up)_.*",
        ])
        .args(["--allowlist-type", "(SDL|AV|Swr|Up)_.*"])
        .args(["--allowlist-var", "(SDL|AV|UP).*"])
        .arg("--no-layout-tests")
        .arg("--no-doc-comments")
        .arg("--formatter")
        .arg("rustfmt")
        .arg("--output")
        .arg(&bindings)
        .arg("--")
        .arg(format!("-I{}", ffmpeg_dir.display()))
        .args(pkg_config("--cflags", &["sdl3"]));
    run(bindgen, "bindgen");

    let mut cc = Command::new("cc");
    cc.arg("-std=c11")
        .args(["-O2", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .args(pkg_config(
            "--cflags",
            &["libplacebo", "sdl3", "pangocairo"],
        ))
        .arg(format!("-I{}", ffmpeg_dir.display()))
        .arg("-c")
        .arg("native/video_renderer.c")
        .arg("-o")
        .arg(&renderer_object);
    run(cc, "C Vulkan renderer compilation");

    let mut input_cc = Command::new("cc");
    input_cc
        .arg("-std=c11")
        .args(["-O2", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .args(pkg_config("--cflags", &["sdl3", "wayland-client"]))
        .arg("-I")
        .arg(&out_dir)
        .arg("-c")
        .arg("native/wayland_input.c")
        .arg("-o")
        .arg(&input_object);
    run(input_cc, "C Wayland input compilation");

    let mut protocol_cc = Command::new("cc");
    protocol_cc
        .args(["-O2", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .arg("-I")
        .arg(&out_dir)
        .arg("-c")
        .arg(&protocol_source)
        .arg("-o")
        .arg(&protocol_object);
    run(protocol_cc, "xdg-shell protocol compilation");

    let mut ar = Command::new("ar");
    ar.arg("crs")
        .arg(&renderer_archive)
        .arg(&renderer_object)
        .arg(&input_object)
        .arg(&protocol_object);
    run(ar, "C Vulkan renderer archive creation");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=video_renderer");

    for library in ["avformat", "avcodec", "swresample", "avutil"] {
        println!("cargo:rustc-link-lib=dylib={library}");
    }
    for argument in pkg_config(
        "--libs",
        &["libplacebo", "sdl3", "wayland-client", "pangocairo"],
    ) {
        if let Some(library) = argument.strip_prefix("-l") {
            println!("cargo:rustc-link-lib=dylib={library}");
        } else if let Some(directory) = argument.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={directory}");
            println!("cargo:rustc-link-arg=-Wl,-rpath,{directory}");
        } else {
            println!("cargo:rustc-link-arg={argument}");
        }
    }

    for directory in ["libavformat", "libavcodec", "libswresample", "libavutil"] {
        let path = ffmpeg_dir.join(directory);
        println!("cargo:rustc-link-search=native={}", path.display());
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", path.display());
    }
}
