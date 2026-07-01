# jj integration decision

`jj-change-viewer` uses the `jj` CLI as its integration boundary. By default it
executes the first `jj` found on `$PATH`, and `[jj].binary` can point at a
specific executable for Nix stores, wrappers, or custom builds.

At startup, the configured binary is probed with `jj --version`. If the
configured path is missing, the app falls back to `jj` on `$PATH`. If neither is
available, it exits with an actionable error explaining how to install jj or set
`[jj].binary` to an absolute path.

The probe closes stdin and has a short timeout so accidentally selecting a
different `jj` package that waits for input cannot hang startup. In Nix configs,
use `nixpkgs#jujutsu`; `nixpkgs#jj` is a different project and can block on
stdin.

Current command shape:

```sh
jj diff --from <base> --to <rev> --git --color=never --no-pager
```

## Why not embed `jj-lib` yet?

`jj-lib` and `jj-cli` are real published Rust crates, but the official jj docs
say there is no definitive integration answer yet and that `jj-lib` is not a
stable API. The roadmap also calls out better Rust APIs for UIs and a possible
RPC API as future work.

Embedding `jj-lib` could avoid parsing command output and expose richer typed
data, but it would also require us to reproduce or depend on CLI behavior for
repo/workspace discovery, config loading, revset resolution, working-copy
snapshotting, copy/rename/conflict handling, and Git-format diff rendering.

## Why the CLI boundary is right for now

- delegates revsets like `trunk()`, `@`, and `@-` to jj itself
- respects the user's installed jj, config, custom builds, and backends
- keeps packaging simpler and avoids coupling to unstable Rust internals
- exactly matches the Git-format diff this app currently parses
- remains easy to debug by copying the emitted command shape

Future work can introduce a backend trait or adopt jj's future RPC/library APIs
if they become stable and provide data we cannot get safely through the CLI.
