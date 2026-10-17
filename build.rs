use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const FFMPEG_PACKAGES: &[&str] = &[
    "libavformat",
    "libavcodec",
    "libswresample",
    "libswscale",
    "libavutil",
];

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

fn link_packages(packages: &[&str]) {
    let arguments = pkg_config("--libs", packages);
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        if argument == "-framework" {
            println!(
                "cargo:rustc-link-lib=framework={}",
                arguments.next().expect("framework name")
            );
        } else if let Some(library) = argument.strip_prefix("-l") {
            println!("cargo:rustc-link-lib=dylib={library}");
        } else if let Some(directory) = argument.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={directory}");
        } else {
            println!("cargo:rustc-link-arg={argument}");
        }
    }
}

fn link_local_ffmpeg(directory: &Path) {
    for package in FFMPEG_PACKAGES {
        let library = package.strip_prefix("lib").expect("FFmpeg library prefix");
        println!("cargo:rustc-link-lib=dylib={library}");
    }
    for package in FFMPEG_PACKAGES {
        let path = directory.join(package);
        println!("cargo:rustc-link-search=native={}", path.display());
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", path.display());
    }
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
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        build_macos(&out_dir, &ffmpeg_cflags, ffmpeg_dir.as_deref());
        return;
    }
    let wayland_protocols_dir = env::var_os("WAYLAND_PROTOCOLS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| pkg_config_path("pkgdatadir", "wayland-protocols"));

    let text_raster_object = out_dir.join("text_raster.o");
    let renderer_object = out_dir.join("video_renderer.o");
    let ffmpeg_compat_object = out_dir.join("ffmpeg_compat.o");
    let platform_object = out_dir.join("platform.o");
    let input_object = out_dir.join("wayland_input.o");
    let mpris_object = out_dir.join("mpris.o");
    let protocol_object = out_dir.join("xdg_shell_protocol.o");
    let renderer_archive = out_dir.join("libvideo_renderer.a");
    let protocol_header = out_dir.join("xdg-shell-client-protocol.h");
    let protocol_source = out_dir.join("xdg-shell-protocol.c");

    println!("cargo:rerun-if-changed=native/text_raster.c");
    println!("cargo:rerun-if-changed=native/text_raster.h");
    println!("cargo:rerun-if-changed=native/video_renderer.c");
    println!("cargo:rerun-if-changed=native/video_renderer.h");
    println!("cargo:rerun-if-changed=native/ffmpeg_compat.c");
    println!("cargo:rerun-if-changed=native/ffmpeg_compat.h");
    println!("cargo:rerun-if-changed=native/ffmpeg_luma.h");
    println!("cargo:rerun-if-changed=native/read_ahead.h");
    println!("cargo:rerun-if-changed=native/hls_input.h");
    println!("cargo:rerun-if-changed=native/platform.c");
    println!("cargo:rerun-if-changed=native/platform.h");
    println!("cargo:rerun-if-changed=native/input_geometry.h");
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

    let mut text_raster_cc = Command::new("cc");
    text_raster_cc
        .args(["-std=c11", "-O2", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .args(pkg_config("--cflags", &["pangocairo"]))
        .arg("-c")
        .arg("native/text_raster.c")
        .arg("-o")
        .arg(&text_raster_object);
    run(text_raster_cc, "Pango text adapter compilation");

    let mut ffmpeg_compat_cc = Command::new("cc");
    ffmpeg_compat_cc
        .arg("-std=c11")
        .args(["-O2", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .args(&ffmpeg_cflags)
        .args(pkg_config("--cflags", &["glib-2.0"]))
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
        .arg(&text_raster_object)
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
    link_packages(&system_packages);
    if let Some(ffmpeg_dir) = ffmpeg_dir {
        link_local_ffmpeg(&ffmpeg_dir);
    }
}

// The macOS path needs neither Wayland protocols, libplacebo nor D-Bus/GIO.
fn build_macos(out_dir: &Path, ffmpeg_cflags: &[String], ffmpeg_dir: Option<&Path>) {
    let sources = [
        "native/video_renderer_macos.c",
        "native/video_color_macos.m",
        "native/video_upscaler_macos.m",
        "native/media_controls_macos.m",
        "native/text_raster.c",
        "native/ffmpeg_compat.c",
        "native/platform.c",
    ];
    for header in [
        "native/video_renderer.h",
        "native/video_color_macos.h",
        "native/video_upscaler_macos.h",
        "native/macos_logging.h",
        "native/media_controls_macos.h",
        "native/text_raster.h",
        "native/ffmpeg_compat.h",
        "native/ffmpeg_luma.h",
        "native/platform.h",
        "native/input_geometry.h",
        "native/window_shape.h",
        "native/read_ahead.h",
        "native/hls_input.h",
    ] {
        println!("cargo:rerun-if-changed={header}");
    }
    println!("cargo:rerun-if-changed=native/video_color_macos.metal");
    let shader =
        std::fs::read_to_string("native/video_color_macos.metal").expect("Metal shader source");
    std::fs::write(
        out_dir.join("video_color_macos_shader.h"),
        format!("static const char video_color_shader[] = {shader:?};\n"),
    )
    .expect("embedded Metal shader");
    let flags = pkg_config("--cflags", &["sdl3", "pangocairo"]);
    let mut objects = Vec::new();
    for source in sources {
        println!("cargo:rerun-if-changed={source}");
        let object = out_dir
            .join(Path::new(source).file_stem().unwrap())
            .with_extension("o");
        let mut cc = Command::new("cc");
        cc.args(["-std=c11", "-O2", "-fPIC", "-Wall", "-Wextra", "-Werror"])
            .arg("-I")
            .arg(out_dir)
            .args(&flags)
            .args(ffmpeg_cflags)
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(&object);
        if source.ends_with(".m") {
            cc.arg("-fobjc-arc");
        }
        run(cc, &format!("macOS compilation of {source}"));
        objects.push(object);
    }
    let mut ar = Command::new("ar");
    ar.arg("crs")
        .arg(out_dir.join("libvideo_renderer.a"))
        .args(objects);
    run(ar, "macOS native archive creation");
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=video_renderer");
    for framework in [
        "VideoToolbox",
        "CoreVideo",
        "CoreMedia",
        "Metal",
        "QuartzCore",
        "Foundation",
        "AppKit",
        "MediaPlayer",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    let mut packages = vec!["sdl3", "pangocairo"];
    if ffmpeg_dir.is_none() {
        packages.extend_from_slice(FFMPEG_PACKAGES);
    }
    link_packages(&packages);
    if let Some(directory) = ffmpeg_dir {
        link_local_ffmpeg(directory);
    }
}
