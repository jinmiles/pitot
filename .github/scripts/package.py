"""Packages the release binaries, each with the licenses of the code compiled into it.

Usage: python .github/scripts/package.py <out dir> [--tag vX.Y.Z]

Run after `cargo build --release --locked -p pitot-overlay -p pitot-bench`. With `--tag`, the tag must match the
workspace version. Writes one zip per binary and SHA256SUMS.txt into the out dir.
"""

import argparse
import hashlib
import json
import pathlib
import subprocess
import sys
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
TARGET = "x86_64-pc-windows-msvc"
BINARIES = ["pitot-overlay", "pitot-bench"]
LICENSE_PREFIXES = ("license", "licence", "copying", "notice", "unlicense")


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


def readme_notices():
    text = (ROOT / "README.md").read_text(encoding="utf-8")
    return text[text.index("## Third-Party Notices") :].strip()


def licenses_text(binary, packages):
    rule = "=" * 78
    lines = [
        f"Third-party licenses for {binary}",
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
        "Rust crates compiled into the binary",
        rule,
        "",
    ]
    lines += [f"- {p['name']} {p['version']}: {p['license']}" for p in packages]
    missing = []
    first_seen = {}
    for p in packages:
        files = license_files(p)
        if not p.get("license") or not files:
            missing.append(f"{p['name']} {p['version']}")
            continue
        lines += ["", rule, f"{p['name']} {p['version']} ({p['license']})"]
        if p.get("repository"):
            lines.append(p["repository"])
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
    for binary in BINARIES:
        exe = release / f"{binary}.exe"
        if not exe.is_file():
            sys.exit(f"{exe} is missing; build the release binaries first")
        packages = linked_packages(meta, binary)
        archive = args.out / f"{binary}-{version}-windows-x64.zip"
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
            z.write(exe, exe.name)
            z.writestr("THIRD-PARTY-LICENSES.txt", licenses_text(binary, packages))
        sums.append(f"{sha256(archive)}  {archive.name}")
        print(f"{archive.name}: {exe.stat().st_size} byte binary, {len(packages)} crates")
    (args.out / "SHA256SUMS.txt").write_text("\n".join(sums) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
