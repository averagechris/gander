use crate::state::{
    ReviewState, ReviewTarget, StepImportance, StepKind, Walkthrough, WalkthroughStep,
};

/// Render persisted local walkthroughs as Markdown.
///
/// This intentionally reads only Gander review state: no server, forge
/// metadata, agent overlay, or working-copy mutation. Agent/zen concepts can be
/// mapped into persisted walkthroughs by a later reconciliation layer.
pub fn render_walkthroughs_markdown(state: &ReviewState) -> String {
    let walkthroughs = state
        .sessions
        .iter()
        .flat_map(|session| session.walkthroughs.iter())
        .collect::<Vec<_>>();

    if walkthroughs.is_empty() {
        return "# Walkthroughs\n\nNo walkthrough steps recorded.\n".to_owned();
    }

    let mut out = String::from("# Walkthroughs\n\n");
    for (index, walkthrough) in walkthroughs.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        write_walkthrough(&mut out, walkthrough);
    }
    out
}

fn write_walkthrough(out: &mut String, walkthrough: &Walkthrough) {
    let title = walkthrough
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("Untitled walkthrough");
    out.push_str("## ");
    out.push_str(title);
    out.push_str("\n\n");

    if walkthrough.steps.is_empty() {
        out.push_str("No steps recorded.\n");
        return;
    }

    for (index, step) in walkthrough.steps.iter().enumerate() {
        write_step(out, index + 1, step);
    }
}

fn write_step(out: &mut String, number: usize, step: &WalkthroughStep) {
    let title = step
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| step.id.trim());
    if step.kind == StepKind::Chapter {
        out.push_str(&format!("## Chapter: {title}\n\n"));
    } else {
        let suffix = if step.importance == StepImportance::Glance {
            " (glance)"
        } else {
            ""
        };
        out.push_str(&format!("### {number}. {title}{suffix}\n\n"));
    }

    if let Some(location) = target_location(&step.target) {
        out.push_str(&format!("Location: `{location}`\n\n"));
    }

    if let Some(why) = step
        .why
        .as_deref()
        .map(str::trim)
        .filter(|why| !why.is_empty())
    {
        out.push_str("Why: ");
        out.push_str(why);
        out.push_str("\n\n");
    }

    if let Some(body) = step
        .body
        .as_deref()
        .map(str::trim)
        .filter(|body| !body.is_empty())
    {
        out.push_str(body);
        out.push_str("\n\n");
    }
    for artifact in &step.artifacts {
        out.push_str(&format!(
            "#### Artifact: {} ({:?})\n\n",
            artifact.title, artifact.kind
        ));
        out.push_str(artifact.body.trim());
        out.push_str("\n\n");
    }
}

fn target_location(target: &ReviewTarget) -> Option<String> {
    let file = target.file.as_deref()?;
    let mut location = match (target.line, target.end_line) {
        (Some(start), Some(end)) if end != start => format!("{file}:{start}-{end}"),
        (Some(line), _) => format!("{file}:{line}"),
        _ => file.to_owned(),
    };
    if let Some(symbol) = target
        .symbol
        .as_deref()
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
    {
        location.push_str(" — ");
        location.push_str(symbol);
    }
    Some(location)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ReviewSession, Walkthrough};

    #[test]
    fn markdown_renders_title_steps_locations_and_why() {
        let mut state = ReviewState::default();
        state.sessions.push(ReviewSession {
            id: "review-1".to_owned(),
            walkthroughs: vec![Walkthrough {
                id: "walkthrough-1".to_owned(),
                title: Some("Persistence tour".to_owned()),
                steps: vec![WalkthroughStep {
                    id: "state".to_owned(),
                    title: Some("Persist the model".to_owned()),
                    body: Some("The walkthrough lives beside comments.".to_owned()),
                    why: Some("Introduces durable review state.".to_owned()),
                    target: ReviewTarget {
                        file: Some("src/state.rs".to_owned()),
                        line: Some(10),
                        end_line: Some(20),
                        symbol: Some("ReviewState".to_owned()),
                        ..ReviewTarget::default()
                    },
                    ..WalkthroughStep::default()
                }],
                ..Walkthrough::default()
            }],
            ..ReviewSession::default()
        });

        let markdown = render_walkthroughs_markdown(&state);

        assert!(markdown.contains("# Walkthroughs"));
        assert!(markdown.contains("## Persistence tour"));
        assert!(markdown.contains("### 1. Persist the model"));
        assert!(markdown.contains("Location: `src/state.rs:10-20 — ReviewState`"));
        assert!(markdown.contains("Why: Introduces durable review state."));
        assert!(markdown.contains("The walkthrough lives beside comments."));
    }

    #[test]
    fn markdown_handles_empty_walkthroughs() {
        assert_eq!(
            render_walkthroughs_markdown(&ReviewState::default()),
            "# Walkthroughs\n\nNo walkthrough steps recorded.\n"
        );
    }
}
