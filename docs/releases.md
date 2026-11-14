# Releases

Releases are built and published by the `Release` workflow (`.github/workflows/release.yml`) when a version tag
is pushed. Nothing is built on a developer machine.

## Making a release

1. In a pull request titled `release: vX.Y.Z`, set `version` in `[workspace.package]` of `Cargo.toml` and run
   `cargo build` once, so `Cargo.lock` records the new version (the workflow builds with `--locked`).
2. Merge it, then tag the merge commit and push the tag:

   ```text
   git fetch origin
   git tag vX.Y.Z origin/main
   git push origin vX.Y.Z
   ```

The workflow then:

- checks that the tagged commit is on `main` and that the tag matches the workspace version,
- runs the tests, and builds `pitot-overlay` and `pitot-bench` in release mode,
- packages them (below) and publishes a GitHub release named `Pitot X.Y.Z` with the packages, short notes on
  running the binaries, and the pull requests merged since the previous release. Versions before 1.0 are marked
  as pre-releases.

## What a release holds

| File | Contents |
|---|---|
| `pitot-overlay-X.Y.Z-windows-x64.zip` | `pitot-overlay.exe` and `THIRD-PARTY-LICENSES.txt` |
| `pitot-bench-X.Y.Z-windows-x64.zip` | `pitot-bench.exe` and `THIRD-PARTY-LICENSES.txt` |
| `SHA256SUMS.txt` | SHA-256 of each zip |

`THIRD-PARTY-LICENSES.txt` lists what is compiled into that binary: the code ported from other projects (the
Third-Party Notices section of `README.md`) and every Rust crate linked in, with its license files.
`.github/scripts/package.py` finds the crates with `cargo metadata` for the Windows target: the binary's normal
dependencies, without proc macros and the crates only they use, since those run at build time and are not in the
binary. Each crate's license files are the ones at the top of its package (`LICENSE*`, `COPYING*`, `NOTICE*`,
`UNLICENSE`); a text that repeats an earlier one word for word is referred to instead of repeated. Packaging fails
when a crate has no license or no license file, so a new dependency without them is caught.

The binaries are not signed yet, and they use the Microsoft Visual C++ runtime (installed with the sim).

## Dry runs

Pull requests that change the workflow, the packaging script, `Cargo.lock`, or `README.md` run the same build and
packaging without publishing; the packages are kept as the run's `release` artifact for seven days. The workflow
can also be started by hand (`workflow_dispatch`) for a dry run of any branch.
