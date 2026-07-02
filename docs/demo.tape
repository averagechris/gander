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
Type "bash scripts/demo-fixture.sh /tmp/gander-demo >/dev/null 2>&1; cd /tmp/gander-demo; PS1='$ '"
Enter
Sleep 6s
Type "clear"
Enter
Sleep 1s
Show

# A quick text summary of the change under review.
Type "gander -b main --generated-preset lockfiles summary"
Sleep 500ms
Enter
Sleep 3s

# Launch the review TUI.
Type "clear"
Enter
Type "gander -b main --generated-preset lockfiles"
Sleep 500ms
Enter
Sleep 2.5s

# Walk the file tree to the parser change.
Type "jjj"
Sleep 1s
Type "j"
Sleep 1.2s

# Focus the diff pane and scroll through the parser change.
Tab
Sleep 1s
Type "jjj"
Sleep 1s
Type "jj"
Sleep 1.5s

# Select a range and leave a line comment.
Type "r"
Sleep 800ms
Type "jj"
Sleep 1s
Type "c"
Sleep 1s
Type "consider a Tag newtype instead of raw strings"
Sleep 1s
Ctrl+S
Sleep 1.5s

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
