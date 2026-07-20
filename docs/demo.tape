# VHS tape for the gander demo GIF.
#
# Render with:
#   nix run .#render-demo
#
# This builds the flake's gander package, puts it (and jj) on PATH, runs vhs
# against this tape, and refreshes docs/demo.gif.inputs-sha256 so CI knows the
# gif is current. CI (.builds/demo.yml) re-renders and commits the gif
# automatically whenever this tape or scripts/demo-fixture.sh changes.
#
# The shell vhs spawns inherits PATH from the wrapper, so the tape itself
# stays machine-independent: it only needs `gander` and `jj` resolvable.

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

# Open gander directly: review starts in the TUI.
Type "gander -b main --generated-preset lockfiles"
Sleep 500ms
Enter
Sleep 2.5s

# Apply Focus in the normal review stream; all review keys remain available.
Type "Z"
Sleep 3s
Type "jj"
Sleep 2s
Type "Z"
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

# Cycle the comment action, then open the tasks popup for actionable follow-up.
Type "a"
Sleep 900ms
Type "X"
Sleep 2s
Escape
Sleep 900ms

# Mark this hunk as a walkthrough step and inspect the walkthrough panel.
Type "Y"
Sleep 1.2s
Type "W"
Sleep 2s
Escape
Sleep 900ms

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
