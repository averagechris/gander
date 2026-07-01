# Release process

gander releases are cut locally with Nix flake apps and published to
SourceHut: canonical `vX.Y.Z` git tags on
[git.sr.ht/~averagechris/gander](https://git.sr.ht/~averagechris/gander),
binary tarballs on a SourceHut Pages downloads site, and an optional
builds.sr.ht job for the Linux x86_64 artifact.

## TL;DR

```sh
# dry-ish run: prepare + validate + tag + local artifact + pages tarball
nix run .#release -- --version 0.2.0

# same, but actually publish the downloads page and kick off the Linux build
nix run .#release -- --version 0.2.0 --publish-pages --submit-linux-build
```

The orchestrator prints the manual follow-up commands for anything you skip.

## Pieces

Each stage is its own flake app so it can be run (and re-run) independently:

| Command | What it does |
| --- | --- |
| `nix run .#prepare-release -- [--version X.Y.Z]` | Writes the version into `Cargo.toml`, `Cargo.lock`, and `.builds/release-linux-x86_64.yml`, then converts the `## Unreleased` section of `CHANGELOG.md` into a dated `## vX.Y.Z` entry. If `Unreleased` is empty, it generates bullets from conventional-commit summaries since the previous semver tag. |
| `nix run .#release-tag -- [--revision REV]` | Creates the `vX.Y.Z` tag from `Cargo.toml` (via `jj tag set` in jj repos) and pushes it to `origin`. Refuses to reuse an existing local or remote tag, and refuses to tag an empty jj revision. |
| `nix build .#release-artifact` | Builds a reproducible tarball for the current platform (`gander-vX.Y.Z-<platform>.tar.gz` containing the binary, `README.md`, `CHANGELOG.md`, and both licenses) plus a `.sha256` checksum file. |
| `nix run .#build-pages -- [--include-existing-downloads]` | Renders `dist/pages/site/` (an `index.html` downloads listing grouped by release, a `manifest.json`, and `downloads/` artifacts from `dist/downloads/`) and packs it into `dist/pages/gander-pages.tar.gz`. `--include-existing-downloads` fetches previously published artifacts from the live `manifest.json` so old releases stay listed. |
| `nix run .#publish-pages` | Publishes the pages tarball with `hut pages publish` to `averagechris.srht.site/gander`. |
| `nix run .#release` | Runs all of the above in order: prepare, `cargo check --locked`, `jj lint`, tag + push (also moves the `main` bookmark and pushes it), local artifact, pages build, and (opt-in) pages publish + Linux build submission. |

## Typical flow

1. Land everything for the release on `main` and note changelog-worthy items
   under `## Unreleased` in `CHANGELOG.md` as you go (preferred over relying
   on generated commit summaries).
2. Run `nix run .#release -- --version X.Y.Z`. This mutates
   `Cargo.toml`/`Cargo.lock`/`CHANGELOG.md`/`.builds/…` in the working copy;
   the changes are snapshotted into `@` by jj and included in the tagged
   revision. Use `--revision @-` if your release commit is already described.
3. Review the result, then publish:
   `nix run .#publish-pages` and/or
   `hut builds submit .builds/release-linux-x86_64.yml …`
   (the release script prints the exact commands).
4. The builds.sr.ht job builds the Linux artifact from the pushed tag,
   rebuilds the pages with `--include-existing-downloads`, and republishes so
   the site gains the Linux tarball alongside the locally built macOS one.

## Notes

- Versions are plain semver `X.Y.Z`; tags are `vX.Y.Z`.
- Requires `hut` (in the dev shell) authenticated for pages/builds publishing,
  and the host `jj lint` alias for the validation stage
  (`--skip-validate` if unavailable).
- Artifact tarballs and the pages tarball are byte-reproducible
  (`--sort=name --mtime=@1 --owner=0 --group=0`, `gzip -n`).
- `dist/` is generated output and stays untracked.
