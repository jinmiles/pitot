"""Packages the release binaries, each with the licenses of the code compiled into it.

Usage: python .github/scripts/package.py <out dir> [--tag vX.Y.Z]

Run after `cargo build --release --locked -p pitot -p pitot-bench`. With `--tag`, the tag must match the workspace
version. Writes into the out dir one zip per package, the binaries published on their own with their license
texts, and SHA256SUMS.txt over all of them.
"""

import argparse
import hashlib
import json
import pathlib
import shutil
import subprocess
import sys
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
TARGET = "x86_64-pc-windows-msvc"
# Each zip and the binaries in it. `pitot` is the overlay and the Pitot window in one executable.
PACKAGES = {"pitot": ["pitot"], "pitot-bench": ["pitot-bench"]}
# Packages whose binary is also published on its own, next to the zips, and the name of its license text there.
SINGLE = {"pitot": "pitot-THIRD-PARTY-LICENSES.txt"}
LICENSE_PREFIXES = ("license", "licence", "copying", "notice", "unlicense")
# License texts for crates whose package has no license file: `<crate>.txt` from the crate's repository, or the
# canonical text of a license the crate offers.
FALLBACK_LICENSES = ROOT / ".github" / "licenses"


def metadata():
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET],
        cwd=ROOT,
        check=True,
        capture_output=True,
    ).stdout
    return json.loads(out)


def linked_packages(meta, binary):
    """Crates compiled into the binary: its normal dependencies, without proc macros, which only run at build
    time (and so neither do the crates only they use)."""
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    members = set(meta["workspace_members"])
    root = next(i for i in members if packages[i]["name"] == binary)
    seen, stack = set(), [root]
    while stack:
        for dep in nodes[stack.pop()]["deps"]:
            package = packages[dep["pkg"]]
            normal = any(kind["kind"] is None for kind in dep["dep_kinds"])
            proc_macro = any("proc-macro" in target["kind"] for target in package["targets"])
            if normal and not proc_macro and dep["pkg"] not in seen:
                seen.add(dep["pkg"])
                stack.append(dep["pkg"])
    linked = (packages[i] for i in seen if i not in members)
    return sorted(linked, key=lambda p: (p["name"], p["version"]))


def license_files(package):
    folder = pathlib.Path(package["manifest_path"]).parent
    return sorted(
        (f for f in folder.iterdir() if f.is_file() and f.name.lower().startswith(LICENSE_PREFIXES)),
        key=lambda f: f.name.lower(),
    )


def fallback_license(package):
    """For a crate published without a license file: its repository's license text kept in FALLBACK_LICENSES, or
    the Apache-2.0 text when the crate offers Apache-2.0 among its licenses. Returns (file, note) or None."""
    own = FALLBACK_LICENSES / f"{package['name']}.txt"
    if own.is_file():
        return own, "Its package has no license file; this is the license file of its repository."
    offered = package.get("license") or ""
    choices = [c.strip() for c in offered.replace("/", " OR ").split(" OR ")]
    if "Apache-2.0" in choices:
        note = "Its package has no license file; it is used under Apache-2.0, one of the licenses it offers."
        return FALLBACK_LICENSES / "Apache-2.0.txt", note
    return None


def readme_notices():
    text = (ROOT / "README.md").read_text(encoding="utf-8")
    return text[text.index("## Third-Party Notices") :].strip()


def licenses_text(binaries, packages):
    rule = "=" * 78
    lines = [
        f"Third-party licenses for {' and '.join(binaries)}",
        "",
        "The Rust standard library is compiled in as well (MIT OR Apache-2.0, https://github.com/rust-lang/rust).",
        "",
        rule,
        "Code ported from other projects (from the repository's README.md)",
        rule,
        "",
        readme_notices(),
        "",
        rule,
        "Rust crates compiled into the binaries",
        rule,
        "",
    ]
    lines += [f"- {p['name']} {p['version']}: {p['license']}" for p in packages]
    missing = []
    first_seen = {}
    for p in packages:
        files = license_files(p)
        note = None
        if p.get("license") and not files and (fallback := fallback_license(p)):
            files, note = [fallback[0]], fallback[1]
        if not p.get("license") or not files:
            missing.append(f"{p['name']} {p['version']}")
            continue
        lines += ["", rule, f"{p['name']} {p['version']} ({p['license']})"]
        if p.get("repository"):
            lines.append(p["repository"])
        if note:
            lines.append(note)
        lines.append(rule)
        for f in files:
            text = f.read_text(encoding="utf-8", errors="replace").strip()
            digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
            if digest in first_seen:
                lines += ["", f"{f.name}: the same text as {first_seen[digest]} above."]
                continue
            first_seen[digest] = f"{f.name} of {p['name']} {p['version']}"
            lines += ["", f"--- {f.name} ---", "", text]
    if missing:
        sys.exit(f"no license or license file for: {', '.join(missing)}")
    return "\n".join(lines) + "\n"


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("out", type=pathlib.Path)
    parser.add_argument("--tag")
    args = parser.parse_args()

    meta = metadata()
    versions = {p["version"] for p in meta["packages"] if p["id"] in meta["workspace_members"]}
    if len(versions) != 1:
        sys.exit(f"workspace crates have different versions: {sorted(versions)}")
    version = versions.pop()
    if args.tag is not None and args.tag != f"v{version}":
        sys.exit(f"tag {args.tag} does not match the workspace version {version}")

    release = pathlib.Path(meta["target_directory"]) / "release"
    args.out.mkdir(parents=True, exist_ok=True)
    sums = []
    for name, binaries in PACKAGES.items():
        exes = [release / f"{binary}.exe" for binary in binaries]
        for exe in exes:
            if not exe.is_file():
                sys.exit(f"{exe} is missing; build the release binaries first")
        linked = {}
        for binary in binaries:
            for package in linked_packages(meta, binary):
                linked[package["id"]] = package
        packages = sorted(linked.values(), key=lambda p: (p["name"], p["version"]))
        licenses = licenses_text(binaries, packages)
        archive = args.out / f"{name}-{version}-windows-x64.zip"
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
            for exe in exes:
                z.write(exe, exe.name)
            z.writestr("THIRD-PARTY-LICENSES.txt", licenses)
        sums.append(f"{sha256(archive)}  {archive.name}")
        sizes = ", ".join(f"{exe.name} {exe.stat().st_size} bytes" for exe in exes)
        print(f"{archive.name}: {sizes}, {len(packages)} crates")
        if name in SINGLE:
            # The same bytes as in the zip.
            single = [args.out / exe.name for exe in exes] + [args.out / SINGLE[name]]
            for exe, copy in zip(exes, single):
                shutil.copyfile(exe, copy)
            single[-1].write_bytes(licenses.encode("utf-8"))
            for path in single:
                sums.append(f"{sha256(path)}  {path.name}")
                print(f"{path.name}: {path.stat().st_size} bytes")
    # Unix line endings, so `sha256sum -c` reads it on any system.
    (args.out / "SHA256SUMS.txt").write_bytes(("\n".join(sums) + "\n").encode("utf-8"))


if __name__ == "__main__":
    main()
