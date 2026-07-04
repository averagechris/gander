# Maintainer-authored onboarding walkthrough

Use walkthroughs when a maintainer wants to guide a new contributor through the
important parts of a change before they dive into every diff hunk.

1. Start the TUI in the review workspace:
   ```sh
   gander tui
   ```
2. Navigate to the first important hunk. Use normal movement, file search `/`,
   symbol outline `o`, or next/previous changed symbol `]`/`[`.
3. Press `Y` to mark the current hunk or selected range as a walkthrough step.
   Gander stores the step in durable review state with a file/line target.
4. Repeat for the conceptual path you want a newcomer to follow: entry point,
   core data model, risky behavior change, and tests.
5. Press `W` to open the walkthrough panel. In the panel:
   - `j`/`k` or arrows move the selection;
   - `enter` jumps to the selected step;
   - `J`/`K` reorders the selected step down/up;
   - `d` deletes a step;
   - `esc` closes the panel.
6. Export the walkthrough for sharing:
   ```sh
   gander walkthrough export > walkthrough.md
   gander export html --output review.html
   ```

The Markdown is useful in chat or docs. The HTML export is a self-contained
review page a new contributor can open locally.
