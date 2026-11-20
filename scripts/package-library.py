#!/usr/bin/env python3
"""Package one JNI library without including the parser's separate cdylib."""

import argparse
import json
from pathlib import Path
import subprocess
import tomllib
import zipfile


LIBRARIES = {
    "x86_64-unknown-linux-gnu": "libvive_converter.so",
    "aarch64-unknown-linux-gnu": "libvive_converter.so",
    "x86_64-pc-windows-msvc": "vive_converter.dll",
    "aarch64-pc-windows-msvc": "vive_converter.dll",
    "aarch64-linux-android": "libvive_converter.so",
    "x86_64-linux-android": "libvive_converter.so",
}
ROOT = Path(__file__).resolve().parent.parent


def git_revision(directory):
    return subprocess.check_output(
        ["git", "-C", str(directory), "rev-parse", "HEAD"], text=True
    ).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=LIBRARIES)
    parser.add_argument("library", type=Path)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    if args.library.name != LIBRARIES[args.target]:
        parser.error(f"expected a library named {LIBRARIES[args.target]}")
    if not args.library.is_file() or args.library.stat().st_size == 0:
        parser.error(f"library is missing or empty: {args.library}")

    with (ROOT / "Cargo.toml").open("rb") as manifest:
        version = tomllib.load(manifest)["package"]["version"]
    with (ROOT / "Cargo.lock").open("rb") as lock:
        parser_version = next(
            package["version"]
            for package in tomllib.load(lock)["package"]
            if package["name"] == "onenote_parser"
        )
    metadata = {
        "version": version,
        "target": args.target,
        "converter_revision": git_revision(ROOT),
        "parser_version": parser_version,
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    }
    args.output_dir.mkdir(parents=True, exist_ok=True)
    archive = args.output_dir / f"vive-converter-{version}-{args.target}.zip"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
        bundle.write(args.library, args.library.name)
        bundle.write(ROOT / "README.md", "README.md")
        bundle.write(ROOT / "LICENSE", "LICENSE")
        bundle.writestr("BUILD-INFO.json", json.dumps(metadata, indent=2) + "\n")
    print(archive)


if __name__ == "__main__":
    main()
