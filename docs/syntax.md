# Syntax highlighting roadmap

`gander` currently ships a built-in tree-sitter registry for common
review languages and lets config enable/disable built-ins or add extra
extension/filename mappings.

The TUI caches syntax highlights by file path, diff fingerprint, and syntax
matching config. Theme changes are applied at render time and do not invalidate
the parsed highlight cache. Unsupported files and highlight failures quietly
fall back to plain diff text.

Future enhancements should use Helix as inspiration, especially its separation
between language detection, grammar sources, query files, and theme capture
mapping. Likely next steps:

1. user-configurable highlight theme/capture styles
2. user-provided query overrides for built-in grammars
3. external grammar loading from local dynamic libraries
4. optional grammar install/build workflow and cache management

External grammars should remain a future subsystem because dynamic loading adds
ABI, packaging, security, and Nix reproducibility concerns.
