# Release process

gander releases are cut locally with the shared fleet release apps (from the
[averagechris.srht.site](https://git.sr.ht/~averagechris/averagechris.srht.site)
flake's `lib.fleet.presets.rust`): canonical `vX.Y.Z` git tags on
[git.sr.ht/~averagechris/gander](https://git.sr.ht/~averagechris/gander) with
binary tarballs attached as tag artifacts, a site refresh job that republishes
the downloads page on averagechris.srht.site, and an optional builds.sr.ht job
for the Linux x86_64 artifact.

## TL;DR

```sh
nix run .#release -- --version X.Y.Z --submit-linux-build
```

## Pieces

Each stage is its own flake app so it can be run (and re-run) independently:

| Command | What it does |
| --- | --- |
| `nix run .#prepare-release -- [--version X.Y.Z]` | Writes the version into `Cargo.toml`, `Cargo.lock`, and `builds/release-linux-x86_64.yml`, converts the `## Unreleased` section of `CHANGELOG.md` into a dated `## vX.Y.Z` entry, then verifies with `cargo check --locked --workspace`. |
| `nix run .#release-tag -- [--revision REV]` | Creates the annotated `vX.Y.Z` tag from `Cargo.toml` and pushes it to `origin`. Refuses to reuse an existing remote tag. |
| `nix build .#release-artifact` | Builds a reproducible tarball for the current platform (`gander-vX.Y.Z-<platform>.tar.gz` containing the binary, `README.md`, `CHANGELOG.md`, and the licenses) plus a `.sha256` checksum file. |
| `nix run .#release` | Runs it all in order: prepare, validate (`ci-fmt`/`ci-clippy`/`ci-test`), tag + push (also moves the `main` bookmark and pushes it), local artifact build, artifact upload to the tag (`hut git artifact upload`), site refresh submit, and (opt-in) Linux build submission. `--skip-validate`, `--skip-tag`, `--skip-artifact`, `--skip-upload`, `--skip-refresh` skip stages. |

Pages for gander (downloads listing, overview/example pages) are rendered and
published by the averagechris.srht.site repo's `refresh-pages` job from the
pushed tags, tag artifacts, and docs — the release flow only submits the
refresh trigger. The legacy per-repo `build-pages` / `publish-pages` apps
remain for manual use during the migration.

## Typical flow

1. Land everything for the release on `main` and note changelog-worthy items
   under `## Unreleased` in `CHANGELOG.md` as you go.
2. Run `nix run .#release -- --version X.Y.Z --submit-linux-build`. This
   mutates `Cargo.toml`/`Cargo.lock`/`CHANGELOG.md`/`builds/…` in the working
   copy; the changes are snapshotted into `@` by jj and included in the tagged
   revision.
3. The builds.sr.ht job builds the Linux artifact from the pushed tag, uploads
   it to the tag, and submits another site refresh so the downloads page gains
   the Linux tarball alongside the locally built macOS one.

## Notes

- Versions are plain semver `X.Y.Z`; tags are `vX.Y.Z`.
- CI (`.builds/ci.yml`: flake check, fmt, clippy, tests) is auto-submitted by
  builds.sr.ht on every push. The release manifest lives in
  `builds/release-linux-x86_64.yml` — deliberately *outside* `.builds/` — so
  artifacts are only built/uploaded when it is submitted explicitly (by
  `nix run .#release -- --submit-linux-build` or manually with
  `hut builds submit`).
- Requires `hut` authenticated for tag-artifact uploads and build submission.
- Run `jj lint` before releasing; the orchestrator's built-in validation only
  covers the `ci-*` apps.
- Artifact tarballs are byte-reproducible
  (`--sort=name --mtime=@1 --owner=0 --group=0`, `gzip -n`).
- `dist/` is generated output and stays untracked.
