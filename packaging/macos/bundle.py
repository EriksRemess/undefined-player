#!/usr/bin/env python3
"""Build a relocatable, locally signed app from the release executable."""
import argparse
import hashlib
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
APP_NAME = "Undefined Player"
BUNDLE_ID = "lv.apps.undefined-player"


def run(*arguments):
    return subprocess.check_output(arguments, text=True).strip()


def sign(path, identity):
    options = [] if identity == "-" else ["--options", "runtime", "--timestamp"]
    run("codesign", "--force", "--sign", identity, *options, str(path))


def system_library(path):
    return path.startswith(("/System/Library/", "/usr/lib/"))


def load_commands(path):
    commands = run("otool", "-l", str(path))
    rpaths = re.findall(r"cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset", commands)
    versions = re.findall(r"\bminos ([\d.]+)", commands)
    return rpaths, versions


def resolve_library(name, owner, executable, rpaths):
    def expand(value):
        return value.replace("@loader_path", str(owner.parent)).replace(
            "@executable_path", str(executable.parent))
    if name.startswith("@rpath/"):
        candidates = [Path(expand(path)) / name.removeprefix("@rpath/") for path in rpaths]
    else:
        candidates = [Path(expand(name))]
    for candidate in candidates:
        if candidate.is_file():
            return candidate.resolve()
    raise RuntimeError(f"Cannot resolve {name} from {owner}")


def copy_libraries(executable, contents):
    frameworks = contents / "Frameworks"
    frameworks.mkdir()
    copied = {}
    names = {}
    versions = [(14, 0)]
    pending = [(executable.resolve(), contents / "MacOS/undefined-player")]
    while pending:
        original, destination = pending.pop()
        rpaths, minimums = load_commands(original)
        versions.extend(tuple(map(int, version.split("."))) for version in minimums)
        changes = []
        dependencies = run("otool", "-L", str(original)).splitlines()[1:]
        for line in dependencies:
            name = line.strip().split(" (compatibility version", 1)[0]
            if system_library(name):
                continue
            resolved = resolve_library(name, original, executable, rpaths)
            if resolved == original:  # dylib's own install name
                continue
            if resolved not in copied:
                basename = resolved.name
                if basename in names and names[basename] != resolved:
                    if hashlib.sha256(resolved.read_bytes()).digest() != hashlib.sha256(names[basename].read_bytes()).digest():
                        raise RuntimeError(f"Conflicting library basename: {basename}")
                else:
                    names[basename] = resolved
                    target = frameworks / basename
                    shutil.copy2(resolved, target)
                    target.chmod(0o755)
                    pending.append((resolved, target))
                copied[resolved] = basename
            prefix = "@loader_path" if destination.parent == frameworks else "@executable_path/../Frameworks"
            changes.extend(["-change", name, f"{prefix}/{copied[resolved]}"])
        for rpath in dict.fromkeys(rpaths):
            if rpath.startswith("/") and not system_library(rpath):
                changes.extend(["-delete_rpath", rpath])
        if destination.parent == frameworks:
            changes.extend(["-id", f"@loader_path/{destination.name}"])
        if changes:
            run("install_name_tool", *changes, str(destination))
    # Keep dependency license notices and Homebrew formula/build metadata.
    notices = contents / "Resources/ThirdParty"
    notices.mkdir()
    for original in copied:
        formula = next((parent for parent in original.parents if (parent / ".brew").is_dir()), None)
        if not formula:
            continue
        target = notices / f"{formula.parent.name}-{formula.name}"
        if target.exists():
            continue
        target.mkdir()
        for path in formula.iterdir():
            if path.is_file() and path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE", "AUTHORS")):
                shutil.copy2(path, target / path.name)
        shutil.copytree(formula / ".brew", target / "homebrew")
    return ".".join(map(str, max(versions))), len(copied)


def document_types():
    imported = []
    for identifier, description, extensions, mime in [
        ("org.matroska.mkv", "Matroska video", ["mkv"], "video/x-matroska"),
        ("org.webmproject.webm", "WebM video", ["webm"], "video/webm"),
        ("org.xiph.ogv", "Ogg video", ["ogv"], "video/ogg"),
        ("com.adobe.flash.video", "Flash video", ["flv", "f4v"], "video/x-flv"),
        ("com.microsoft.windows-media-wmv", "Windows Media video", ["wmv", "asf"], "video/x-ms-wmv"),
        ("public.mpeg-2-transport-stream", "MPEG transport stream", ["ts", "mts", "m2ts"], "video/mp2t"),
    ]:
        imported.append({"UTTypeIdentifier": identifier, "UTTypeDescription": description,
                         "UTTypeConformsTo": ["public.movie"],
                         "UTTypeTagSpecification": {"public.filename-extension": extensions,
                                                    "public.mime-type": mime}})
    types = ["public.movie", "public.video", "public.mpeg-4", "com.apple.m4v-video",
             "com.apple.quicktime-movie", "public.avi", "public.mpeg", "public.3gpp", "public.3gpp2"]
    types.extend(item["UTTypeIdentifier"] for item in imported)
    return [{"CFBundleTypeName": "Video", "CFBundleTypeRole": "Viewer",
             "LSHandlerRank": "Alternate", "LSItemContentTypes": types}], imported


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/undefined-player")
    parser.add_argument("--output", type=Path, default=ROOT / "target/dist" / f"{APP_NAME}.app")
    parser.add_argument("--identity", default="-", help="codesign identity; defaults to ad-hoc signing (-)")
    args = parser.parse_args()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="macos-bundle-", dir=args.output.parent) as temporary:
        app = Path(temporary) / f"{APP_NAME}.app"
        contents = app / "Contents"
        (contents / "MacOS").mkdir(parents=True)
        (contents / "Resources").mkdir()
        shutil.copy2(args.binary, contents / "MacOS/undefined-player")
        minimum, libraries = copy_libraries(args.binary, contents)
        iconset = Path(temporary) / "AppIcon.iconset"
        iconset.mkdir()
        icon_tool = Path(temporary) / "make-icon"
        run("cc", "-fobjc-arc", "-Wall", "-Wextra", "-Werror", str(ROOT / "packaging/macos/icon.m"),
            "-framework", "AppKit", "-o", str(icon_tool))
        run(str(icon_tool), str(iconset))
        run("iconutil", "-c", "icns", str(iconset), "-o", str(contents / "Resources/AppIcon.icns"))
        shutil.copy2(ROOT / "packaging/macos/fonts.conf", contents / "Resources/fonts.conf")
        documents, imported = document_types()
        info = {"CFBundleExecutable": "undefined-player", "CFBundleIdentifier": BUNDLE_ID,
                "CFBundleName": APP_NAME, "CFBundleDisplayName": APP_NAME,
                "CFBundlePackageType": "APPL", "CFBundleShortVersionString": version,
                "CFBundleIconFile": "AppIcon.icns",
                "CFBundleVersion": version, "LSMinimumSystemVersion": minimum,
                "NSHighResolutionCapable": True, "CFBundleDocumentTypes": documents,
                "UTImportedTypeDeclarations": imported,
                "NSHumanReadableCopyright": "Copyright Ēriks Remess. GPL-3.0-or-later."}
        with (contents / "Info.plist").open("wb") as output:
            plistlib.dump(info, output)
        for name in ["LICENSE", "README.md", "USAGE.md", "CONFIGURATION.md"]:
            shutil.copy2(ROOT / name, contents / "Resources" / name)
        for library in sorted((contents / "Frameworks").iterdir()):
            sign(library, args.identity)
        sign(app, args.identity)
        run("codesign", "--verify", "--deep", "--strict", str(app))
        # Only replace the explicitly selected build artifact after validation.
        if args.output.exists():
            shutil.rmtree(args.output)
        shutil.move(str(app), args.output)
    print(f"Built {args.output} ({libraries} bundled libraries, macOS {minimum}+)")


if __name__ == "__main__":
    main()
