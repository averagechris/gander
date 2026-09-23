# Release process

New gander releases are GitHub-only. The local release app comes from the
SHA-pinned `averagechris/fleet` `lib.fleet.presets.gander` preset. It prepares
and validates the release commit, then atomically publishes `main` and its
annotated `vX.Y.Z` tag to `averagechris/gander`. The tag starts the thin caller
in `.github/workflows/release.yml`; the SHA-pinned fleet workflow builds:

- `aarch64-darwin` on `macos-14`
- `x86_64-linux` on `ubuntu-24.04`

Each platform contributes an archive and checksum. The GitHub release remains
a draft until both configured pairs exist and verify. After publication, the
workflow dispatches `pages.yml` in `averagechris/averagechris.github.io` using
the GitHub App ID in `WEBSITE_APP_ID` and private key in
`WEBSITE_APP_PRIVATE_KEY`. Until those are configured, the workflow emits an
explicit warning that website dispatch was skipped.

## Commands

```sh
nix run --accept-flake-config .#release -- --version X.Y.Z --check
nix run --accept-flake-config .#release -- --version X.Y.Z
```

Run these from a fresh empty `@` whose parent exactly matches local `main` and
`main@origin`. `--check` is a nonmutating ref/version preflight only: it checks
the requested version and release refs, but does not run validation or build
the release artifact. The real release command updates `Cargo.toml`,
`Cargo.lock`, and `CHANGELOG.md`, runs the configured fmt, Clippy, test, and
release-contract gates, verifies the reproducible artifact, creates an
annotated tag, and atomically pushes the release commit and tag. It does not
upload assets itself; GitHub Actions is the sole release and asset writer.

The workflow does not run on a `main` push. To recover an existing release,
dispatch it from the default branch (for example,
`gh workflow run release.yml --ref main -f tag=v0.8.3`). The requested tag must
exist on `origin`, be annotated, and peel to a commit that is an ancestor of
the selected `main` commit. This recovery mode never moves the tag and disables
website dispatch; use the hourly or manual site refresh afterward. Tag pushes
still require the event SHA to equal the tag's peeled commit. Unknown tags,
malformed tags, forks, pull requests, and mismatched refs fail closed.

## Historical SourceHut releases

SourceHut release artifacts and `builds/release-linux-x86_64.yml` are archival
for tags published before this migration. Do not dual-publish new tags and do
not submit that manifest for a new release. Historical source and artifacts
remain at <https://git.sr.ht/~averagechris/gander>.

## Notes

- Versions are plain semver `X.Y.Z`; release tags are `vX.Y.Z`.
- Run `jj lint` before releasing. There are no release-stage bypass flags.
- Artifact tarballs are byte-reproducible (`--sort=name --mtime=@1 --owner=0
  --group=0`, `gzip -n`).
- `dist/` is generated output and stays untracked.
