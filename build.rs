use std::env;
use std::path::PathBuf;
use std::process::Command;

const FFMPEG_PACKAGES: &[&str] = &["libavformat", "libavcodec", "libswresample", "libavutil"];

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

    let ffmpeg_dir = env::var_os("FFMPEG_DIR").map(PathBuf::from);
    let ffmpeg_cflags = ffmpeg_dir.as_ref().map_or_else(
        || pkg_config("--cflags", FFMPEG_PACKAGES),
        |directory| vec![format!("-I{}", directory.display())],
    );
    let wayland_protocols_dir = env::var_os("WAYLAND_PROTOCOLS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| pkg_config_path("pkgdatadir", "wayland-protocols"));

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let renderer_object = out_dir.join("video_renderer.o");
    let ffmpeg_compat_object = out_dir.join("ffmpeg_compat.o");
    let platform_object = out_dir.join("platform.o");
    let input_object = out_dir.join("wayland_input.o");
    let mpris_object = out_dir.join("mpris.o");
    let protocol_object = out_dir.join("xdg_shell_protocol.o");
    let renderer_archive = out_dir.join("libvideo_renderer.a");
    let protocol_header = out_dir.join("xdg-shell-client-protocol.h");
    let protocol_source = out_dir.join("xdg-shell-protocol.c");

    println!("cargo:rerun-if-changed=native/video_renderer.c");
    println!("cargo:rerun-if-changed=native/video_renderer.h");
    println!("cargo:rerun-if-changed=native/ffmpeg_compat.c");
    println!("cargo:rerun-if-changed=native/ffmpeg_compat.h");
    println!("cargo:rerun-if-changed=native/platform.c");
    println!("cargo:rerun-if-changed=native/platform.h");
    println!("cargo:rerun-if-changed=native/wayland_input.c");
    println!("cargo:rerun-if-changed=native/wayland_input.h");
    println!("cargo:rerun-if-changed=native/mpris.c");
    println!("cargo:rerun-if-changed=native/mpris.h");

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

    let mut cc = Command::new("cc");
    cc.arg("-std=c11")
        .args(["-O2", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .args(pkg_config(
            "--cflags",
            &["libplacebo", "sdl3", "pangocairo"],
        ))
        .args(&ffmpeg_cflags)
        .arg("-c")
        .arg("native/video_renderer.c")
        .arg("-o")
        .arg(&renderer_object);
    run(cc, "C Vulkan renderer compilation");

    let mut ffmpeg_compat_cc = Command::new("cc");
    ffmpeg_compat_cc
        .arg("-std=c11")
        .args(["-O2", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .args(&ffmpeg_cflags)
        .arg("-c")
        .arg("native/ffmpeg_compat.c")
        .arg("-o")
        .arg(&ffmpeg_compat_object);
    run(ffmpeg_compat_cc, "C FFmpeg compatibility layer compilation");

    let mut platform_cc = Command::new("cc");
    platform_cc
        .arg("-std=c11")
        .args(["-O2", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .args(pkg_config("--cflags", &["sdl3"]))
        .arg("-c")
        .arg("native/platform.c")
        .arg("-o")
        .arg(&platform_object);
    run(platform_cc, "C platform compatibility layer compilation");

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

    let mut mpris_cc = Command::new("cc");
    mpris_cc
        .arg("-std=c11")
        .args(["-O2", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .args(pkg_config("--cflags", &["gio-2.0"]))
        .arg("-c")
        .arg("native/mpris.c")
        .arg("-o")
        .arg(&mpris_object);
    run(mpris_cc, "C MPRIS bridge compilation");

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
        .arg(&ffmpeg_compat_object)
        .arg(&platform_object)
        .arg(&input_object)
        .arg(&mpris_object)
        .arg(&protocol_object);
    run(ar, "C Vulkan renderer archive creation");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=video_renderer");

    let mut system_packages = vec![
        "libplacebo",
        "sdl3",
        "wayland-client",
        "pangocairo",
        "gio-2.0",
    ];
    if ffmpeg_dir.is_none() {
        system_packages.extend_from_slice(FFMPEG_PACKAGES);
    }
    for argument in pkg_config("--libs", &system_packages) {
        if let Some(library) = argument.strip_prefix("-l") {
            println!("cargo:rustc-link-lib=dylib={library}");
        } else if let Some(directory) = argument.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={directory}");
        } else {
            println!("cargo:rustc-link-arg={argument}");
        }
    }

    if let Some(ffmpeg_dir) = ffmpeg_dir {
        for library in ["avformat", "avcodec", "swresample", "avutil"] {
            println!("cargo:rustc-link-lib=dylib={library}");
        }
        for directory in ["libavformat", "libavcodec", "libswresample", "libavutil"] {
            let path = ffmpeg_dir.join(directory);
            println!("cargo:rustc-link-search=native={}", path.display());
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", path.display());
        }
    }
}
