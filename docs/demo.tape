# VHS tape for the gander demo GIF.
#
# Record with:
#   nix develop -c cargo build --release
#   PATH="$PWD/target/release:$PATH" nix run nixpkgs#vhs -- docs/demo.tape
#
# Requires `jj` on PATH (dev shell provides it).

Output docs/demo.gif

Set FontSize 15
Set Width 1200
Set Height 720
Set Padding 12
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms
Set Shell bash

Hide
Type "export PATH='/Users/chris/projects/gander/target/release':$PATH; bash scripts/demo-fixture.sh /tmp/gander-demo >/dev/null 2>&1; cd /tmp/gander-demo; PS1='$ '"
Enter
Sleep 6s
Type "clear"
Enter
Sleep 1s
Show

# Open gander directly: review starts in the TUI.
Type "gander -b main --generated-preset lockfiles"
Sleep 500ms
Enter
Sleep 2.5s

# Start with zen mode: a focused walkthrough of the change.
Type "T"
Sleep 3s
Enter
Sleep 2s
Escape
Sleep 1s

# Jump straight to the parser change, then use the changed-symbol outline.
Type "/"
Sleep 800ms
Type "parser"
Sleep 800ms
Enter
Sleep 1s
Tab
Sleep 800ms
Type "o"
Sleep 1.8s
Enter
Sleep 1.2s

# Toggle side-by-side and hide the tree for a focused read.
Type "|"
Sleep 1.2s
Type "w"
Sleep 1.5s
Type "jjjjjj"
Sleep 1.5s
Type "w"
Sleep 800ms

# Select the normalization lines and leave a relevant review comment.
Type "r"
Sleep 800ms
Type "jjj"
Sleep 1s
Type "c"
Sleep 1s
Type "lowercasing is a behavior change; keep the README note and tests in sync"
Sleep 1.6s
Ctrl+S
Sleep 2s

# Back to the tree; mark files viewed as we go.
Tab
Sleep 800ms
Enter
Sleep 1s
Enter
Sleep 1s

# Jump to the next unviewed file, then mark everything viewed.
Type "n"
Sleep 1.2s
Type "a"
Sleep 1.5s

# Quit; the review artifact (with our comment) prints to stdout.
Type "q"
Sleep 4.5s
