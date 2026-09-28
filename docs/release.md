# Release process

New gander releases are GitHub-only. The local release app comes from the
SHA-pinned `averagechris/fleet` `lib.fleet.presets.gander` preset. It prepares
and validates the release commit, then atomically publishes `main` and its
annotated `vX.Y.Z` tag to `averagechris/gander`. The tag starts the thin caller
in `.github/workflows/release.yml`; the SHA-pinned fleet workflow builds:

- `aarch64-darwin` on `macos-14`
- `x86_64-linux` on `ubuntu-24.04`

Each platform contributes an archive and checksum to an Actions artifact named
`release-<platform>`. A green workflow means only that all artifacts are ready;
an operator must publish the GitHub Release and refresh the website manually.

## Commands

```bash
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
upload assets, create a GitHub Release, or dispatch the website.

The workflow does not run on a `main` push. To recover an existing release,
dispatch it from the default branch (for example,
`gh workflow run release.yml --ref main -f tag=v0.8.3`). The requested tag must
exist on `origin`, be annotated, and peel to a commit that is an ancestor of
the selected `main` commit. This recovery mode never moves the tag and disables
automatic publication. Tag pushes still require the event SHA to equal the
tag's peeled commit. Unknown tags,
malformed tags, forks, pull requests, and mismatched refs fail closed.

## Publish the built artifacts

After the workflow succeeds, use a locally authorized `gh` session (run
`gh auth refresh -h github.com -s workflow` if dispatch permission is absent):

```sh
tag=vX.Y.Z
run_id=123456789
rm -rf "dist/manual-$tag" && mkdir -p "dist/manual-$tag"
gh run download "$run_id" -n release-aarch64-darwin -D "dist/manual-$tag/aarch64-darwin"
gh run download "$run_id" -n release-x86_64-linux -D "dist/manual-$tag/x86_64-linux"

remote="$(git ls-remote --tags origin "refs/tags/$tag" "refs/tags/$tag^{}")"
tag_object="$(awk -v r="refs/tags/$tag" '$2 == r {print $1}' <<<"$remote")"
commit="$(awk -v r="refs/tags/$tag^{}" '$2 == r {print $1}' <<<"$remote")"
test -n "$tag_object" && test -n "$commit"
test "$(git cat-file -t "$tag")" = tag
test "$(git rev-parse "$tag^{tag}")" = "$tag_object"
test "$(git rev-parse "$tag^{commit}")" = "$commit"

assets=()
while IFS= read -r asset; do assets+=("$asset"); done \
  < <(find "dist/manual-$tag" -type f -print | sort)
test "${#assets[@]}" -eq 4
test "$(find "dist/manual-$tag" -type f -name '*.tar.gz' | wc -l | tr -d ' ')" -eq 2
test "$(find "dist/manual-$tag" -type f -name '*.tar.gz.sha256' | wc -l | tr -d ' ')" -eq 2
(cd "dist/manual-$tag/aarch64-darwin" && sha256sum -c -- *.sha256)
(cd "dist/manual-$tag/x86_64-linux" && sha256sum -c -- *.sha256)

gh release create "$tag" --verify-tag --draft --title "$tag" --notes-from-tag
gh release upload "$tag" "${assets[@]}" # intentionally no --clobber
rm -rf "dist/remote-$tag" && mkdir -p "dist/remote-$tag"
gh release download "$tag" -D "dist/remote-$tag"
for asset in "${assets[@]}"; do cmp "$asset" "dist/remote-$tag/$(basename "$asset")"; done
gh release edit "$tag" --draft=false

gh workflow run pages.yml --repo averagechris/averagechris.github.io \
  -f tag="$tag" -f sha="$commit"
```

Do not undraft until all four remote assets compare byte-for-byte. After the
site workflow is green, verify its live release label, download URLs, and both
displayed checksums against the sidecars.

`v0.8.3` was recovered with this manual procedure and its site was updated.
The unattended publisher was deliberately removed after its `GITHUB_TOKEN`
release write received HTTP 403; this pipeline does not publish releases.

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
