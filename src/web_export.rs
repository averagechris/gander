use std::collections::BTreeSet;

use crate::{
    app::ReviewSession,
    artifact::{ArtifactProfile, ReviewArtifact},
    state::{ActionItem, AuthorKind, Channel, Comment, ReviewState, ReviewTarget},
};

#[allow(dead_code)]
pub fn render_html(session: &ReviewSession, state: &ReviewState) -> String {
    render_html_with_profile(session, state, ArtifactProfile::Human)
}

pub fn render_html_with_profile(
    session: &ReviewSession,
    state: &ReviewState,
    profile: ArtifactProfile,
) -> String {
    let attention_files = session
        .files
        .iter()
        .map(|file| file.diff.clone())
        .collect::<Vec<_>>();
    render_html_with_profile_and_attention_files(session, state, profile, &attention_files)
}

pub fn render_html_with_profile_and_attention_files(
    session: &ReviewSession,
    state: &ReviewState,
    profile: ArtifactProfile,
    _attention_files: &[crate::diff::FileDiff],
) -> String {
    render_html_with_profile_attention_files_and_theme(
        session,
        state,
        profile,
        _attention_files,
        &crate::config::ThemeConfig::default(),
    )
}

pub fn render_html_with_profile_attention_files_and_theme(
    session: &ReviewSession,
    state: &ReviewState,
    profile: ArtifactProfile,
    _attention_files: &[crate::diff::FileDiff],
    theme: &crate::config::ThemeConfig,
) -> String {
    let team = profile == ArtifactProfile::Team;
    let projected_team_session = team.then(|| {
        let mut projected = session.clone();
        projected.comments = state.comments.clone();
        *projected.durable_sessions_mut() = state.sessions.clone();
        projected
    });
    let team_artifact = projected_team_session
        .as_ref()
        .map(|projected| ReviewArtifact::build(projected, ArtifactProfile::Team));
    let mut projected = session.clone();
    projected.apply_review_state(state.clone());
    if let Some(artifact) = &team_artifact {
        let mut publication = projected.to_state();
        publication.comments = artifact
            .comments
            .iter()
            .map(|comment| comment.comment.clone())
            .collect();
        if let Some(active) = active_durable_session(session, state)
            && let Some(durable) = publication
                .sessions
                .iter_mut()
                .find(|candidate| candidate.id == active.id)
        {
            durable.walkthroughs.clear();
            durable.attention_regions.clear();
            durable.attention_progress.clear();
            durable.action_items.clear();
        }
        projected.apply_review_state(publication);
    }
    let mut view = crate::web_render::GuideView::from_session(&projected);
    if team {
        if let Some(artifact) = &team_artifact {
            view.projection.summary.clone_from(&artifact.summary);
        }
        for file in &mut view.files {
            file.viewed = false;
        }
    }
    let active_session = active_durable_session(session, state);
    let active_session_id = active_session.map(|durable| durable.id.as_str());
    let linked_comment_ids = if let Some(artifact) = &team_artifact {
        artifact
            .comments
            .iter()
            .map(|comment| comment.comment.id.as_str())
            .collect::<std::collections::BTreeSet<_>>()
    } else {
        active_session
            .into_iter()
            .flat_map(|durable| durable.action_items.iter())
            .flat_map(|item| item.comment_ids.iter().map(String::as_str))
            .collect::<BTreeSet<_>>()
    };
    let mut options = crate::web_render::RenderOptions::static_artifact();
    options.show_private_progress = !team;
    let mut out = String::from(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>gander review export</title><script>",
    );
    out.push_str(crate::web_render::PREPAINT_SCRIPT);
    out.push_str("</script><style>");
    out.push_str(&crate::web_render::render_theme_css(theme));
    out.push_str(crate::web_render::COMPONENT_CSS);
    out.push_str("</style></head><body data-mode=\"guided\"><header class=\"topbar\"><div><p class=\"eyebrow\">Gander static review</p><strong>");
    esc_to(&mut out, &session.target.to_string());
    out.push_str("</strong></div><div class=\"controls\"><button class=\"theme-toggle\" type=\"button\" data-theme-toggle>Theme: <span data-theme-label>system</span></button><button id=\"mode-switch\" type=\"button\" aria-pressed=\"false\">Full review</button></div></header><div class=\"app-layout\">");
    out.push_str(&crate::web_render::render_file_tree(&view, options));
    out.push_str("<main>");
    out.push_str(&crate::web_render::render_overview(
        &view,
        &view.target,
        options,
    ));
    if team {
        if let Some(disposition) = team_artifact
            .as_ref()
            .and_then(|a| a.session.as_ref())
            .and_then(|s| s.disposition)
        {
            out.push_str("<section class=\"card\"><h2>Disposition</h2><p>");
            esc_to(
                &mut out,
                serde_json::to_string(&disposition)
                    .unwrap()
                    .trim_matches('"'),
            );
            out.push_str("</p></section>");
        }
    } else {
        if !projection_has_walkthrough(&view) {
            render_walkthroughs(&mut out, active_session);
        }
        // Keep the durable assignment ledger as export-only provenance. The
        // guide itself still comes exclusively from the reading projection.
        render_attention_regions(&mut out, active_session);
        render_action_items(&mut out, active_session, &state.comments);
    }
    let team_comments = team_artifact.as_ref().map(|artifact| {
        artifact
            .comments
            .iter()
            .map(|comment| comment.comment)
            .collect::<Vec<_>>()
    });
    let general_comments = if let Some(comments) = &team_comments {
        comments
            .iter()
            .copied()
            .filter(|comment| comment.path.is_none())
            .collect::<Vec<_>>()
    } else {
        state
            .comments
            .iter()
            .filter(|comment| comment_belongs_to_session(comment, active_session_id))
            .filter(|comment| !linked_comment_ids.contains(comment.id.as_str()))
            .filter(|comment| comment.path.is_none())
            .collect::<Vec<_>>()
    };
    if !general_comments.is_empty() {
        out.push_str("<section class=\"card comments\"><h2>General comments</h2>");
        for comment in general_comments {
            render_comment(&mut out, comment);
        }
        out.push_str("</section>");
    }
    let projected_comment_ids = projected_comment_ids(&view);
    let file_comments = if let Some(comments) = &team_comments {
        comments
            .iter()
            .copied()
            .filter(|comment| comment.path.is_some())
            .filter(|comment| !projected_comment_ids.contains(comment.id.as_str()))
            .collect::<Vec<_>>()
    } else {
        state
            .comments
            .iter()
            .filter(|comment| comment_belongs_to_session(comment, active_session_id))
            .filter(|comment| !linked_comment_ids.contains(comment.id.as_str()))
            .filter(|comment| comment.path.is_some())
            .filter(|comment| !projected_comment_ids.contains(comment.id.as_str()))
            .collect::<Vec<_>>()
    };
    if !file_comments.is_empty() {
        out.push_str("<section class=\"card comments\"><h2>Unanchored file comments</h2>");
        for comment in file_comments {
            render_comment(&mut out, comment);
        }
        out.push_str("</section>");
    }
    out.push_str("<section id=\"review-stream\" class=\"review-stream\" aria-label=\"Shared review stream\"><div class=\"stream-heading\"><div><p class=\"eyebrow\">Shared projection</p><h2>Review stream</h2></div><p class=\"guided-only\">Skims stay compact; spotlights carry narration.</p><p class=\"full-only\">Every file and line is visible. Salience remains in the margin.</p></div>");
    for region in &view.projection.regions {
        out.push_str(&crate::web_render::render_region(
            region,
            crate::web_render::RenderMode::Full,
            options,
        ));
    }
    out.push_str("</section>");
    out.push_str(&crate::web_render::render_footer(&view));
    out.push_str("</main></div><script>");
    out.push_str(crate::web_render::THEME_CONTROL_SCRIPT);
    out.push_str("</script><script>");
    out.push_str(include_str!("web_static.js"));
    out.push_str("</script></body></html>\n");
    out
}

fn projected_comment_ids(view: &crate::web_render::GuideView) -> BTreeSet<&str> {
    view.projection
        .regions
        .iter()
        .flat_map(|region| &region.rows)
        .flat_map(|row| &row.annotations)
        .filter_map(|annotation| match &annotation.source {
            crate::app::ReadingAnnotationSource::Comment(comment) => Some(comment.id.as_str()),
            crate::app::ReadingAnnotationSource::Walkthrough { .. } => None,
        })
        .collect()
}

fn projection_has_walkthrough(view: &crate::web_render::GuideView) -> bool {
    view.projection
        .regions
        .iter()
        .flat_map(|region| &region.rows)
        .flat_map(|row| &row.annotations)
        .any(|annotation| {
            matches!(
                annotation.source,
                crate::app::ReadingAnnotationSource::Walkthrough { .. }
            )
        })
}

fn identity_kind(kind: AuthorKind) -> &'static str {
    match kind {
        AuthorKind::Human => "human",
        AuthorKind::Agent => "agent",
    }
}

fn channel_label(channel: Channel) -> &'static str {
    match channel {
        Channel::Onboarding => "onboarding",
        Channel::Delegation => "delegation",
        Channel::Collaboration => "collaboration",
        Channel::Note => "note",
    }
}

fn active_durable_session<'a>(
    session: &ReviewSession,
    state: &'a ReviewState,
) -> Option<&'a crate::state::ReviewSession> {
    crate::review::active_session_for_loaded_review(
        &state.sessions,
        &session.repo,
        &session.target.base,
        &session.target.rev,
    )
}

fn comment_belongs_to_session(comment: &Comment, session_id: Option<&str>) -> bool {
    session_id.map_or(comment.session_id.is_none(), |id| {
        comment.belongs_to_session(id)
    })
}

/// Compatibility summary for legacy walkthroughs that cannot enter the
/// fingerprint-current reading projection. Current guides render inline via
/// `web_render`; this path prevents older artifacts from silently losing data.
fn render_walkthroughs(out: &mut String, session: Option<&crate::state::ReviewSession>) {
    let steps = session
        .into_iter()
        .flat_map(|session| &session.walkthroughs)
        .flat_map(|walkthrough| &walkthrough.steps)
        .collect::<Vec<_>>();
    if steps.is_empty() {
        return;
    }
    out.push_str("<section class=\"card\"><h2>Legacy walkthrough</h2><ol>");
    for step in steps {
        out.push_str("<li><h3>");
        esc_to(out, step.title.as_deref().unwrap_or("Review step"));
        out.push_str("</h3>");
        if let Some(why) = &step.why {
            out.push_str("<p><strong>Why:</strong> ");
            esc_to(out, why);
            out.push_str("</p>");
        }
        if let Some(body) = &step.body {
            out.push_str("<p>");
            esc_to(out, body);
            out.push_str("</p>");
        }
        render_target_link(out, &step.target);
        out.push_str("</li>");
    }
    out.push_str("</ol></section>");
}

fn render_attention_regions(out: &mut String, durable: Option<&crate::state::ReviewSession>) {
    let Some(durable) = durable else { return };
    if durable.attention_regions.is_empty() {
        return;
    }
    out.push_str("<section class=\"card attention\"><h2>Attention assignments</h2><ul>");
    for region in &durable.attention_regions {
        out.push_str("<li><strong>");
        esc_to(out, &format!("{:?}", region.salience).to_lowercase());
        out.push_str("</strong> <span class=\"pill\">");
        esc_to(out, &format!("{:?}", region.source).to_lowercase());
        out.push_str("</span>");
        render_target_link(out, &region.target);
        if let Some(rationale) = &region.rationale {
            out.push_str("<p>");
            esc_to(out, rationale);
            out.push_str("</p>");
        }
        out.push_str("</li>");
    }
    out.push_str("</ul></section>");
}

fn render_action_items(
    out: &mut String,
    session: Option<&crate::state::ReviewSession>,
    comments: &[Comment],
) {
    let Some(session) = session else {
        return;
    };
    if session.action_items.is_empty() {
        return;
    }
    out.push_str("<section class=\"card\"><h2>Action items</h2>");
    for item in &session.action_items {
        render_action_item(out, item, session, comments);
    }
    out.push_str("</section>");
}

fn render_action_item(
    out: &mut String,
    item: &ActionItem,
    session: &crate::state::ReviewSession,
    comments: &[Comment],
) {
    out.push_str("<article class=\"comment action-item\"><div><span class=\"pill\">");
    esc_to(out, &format!("{:?}", item.status).to_lowercase());
    out.push_str("</span><span class=\"pill action\">");
    esc_to(
        out,
        &item
            .action
            .map(|action| format!("{action:?}").to_lowercase())
            .unwrap_or_else(|| "none".into()),
    );
    out.push_str("</span></div><h3>");
    esc_to(out, &item.title);
    out.push_str("</h3>");
    if let Some(body) = &item.body {
        out.push_str("<p>");
        esc_to(out, body);
        out.push_str("</p>");
    }
    if let Some(target) = &item.target {
        render_target_link(out, target);
    }
    if let Some(disposition) = item.disposition {
        out.push_str("<p><strong>Disposition:</strong> ");
        esc_to(out, &format!("{disposition:?}").to_lowercase());
        out.push_str("</p>");
    }
    if let Some(outcome) = &item.outcome {
        out.push_str("<p><strong>Outcome:</strong> ");
        esc_to(out, outcome);
        out.push_str("</p>");
    }
    if let Some(closed_at) = item.closed_at {
        out.push_str("<p><strong>Closed:</strong> ");
        esc_to(out, &closed_at.to_rfc3339());
        out.push_str("</p>");
    }
    for ticket in &item.external_tickets {
        out.push_str("<p class=\"external-ticket\"><strong>External ticket:</strong> ");
        esc_to(out, &ticket.tracker);
        out.push(' ');
        esc_to(out, &ticket.reference);
        if let Some(url) = &ticket.url {
            out.push_str(" (");
            esc_to(out, url);
            out.push(')');
        }
        out.push_str("</p>");
    }
    let linked_comments = item
        .comment_ids
        .iter()
        .filter_map(|id| {
            comments
                .iter()
                .find(|comment| comment.id == *id && comment.belongs_to_session(&session.id))
        })
        .collect::<Vec<_>>();
    if !linked_comments.is_empty() {
        out.push_str("<div class=\"comments evidence\"><h4>Evidence comments</h4>");
        for comment in linked_comments {
            render_comment(out, comment);
        }
        out.push_str("</div>");
    }
    out.push_str("</article>");
}

fn render_comment(out: &mut String, comment: &Comment) {
    out.push_str("<article class=\"comment\"><div><span class=\"pill\">");
    esc_to(out, comment.state.label());
    out.push_str("</span>");
    if let Some(kind) = comment.kind {
        out.push_str("<span class=\"pill\">");
        esc_to(out, &format!("{kind:?}").to_lowercase());
        out.push_str("</span>");
    }
    if let Some(action) = comment.action {
        out.push_str("<span class=\"pill action\">");
        esc_to(out, &format!("{action:?}").to_lowercase());
        out.push_str("</span>");
    }
    out.push_str("<span class=\"pill\">id ");
    esc_to(out, &comment.id[..comment.id.len().min(8)]);
    out.push_str("</span><span class=\"pill\">author ");
    esc_to(
        out,
        &format!(
            "{}:{}",
            identity_kind(comment.author.kind),
            comment.author.name
        ),
    );
    out.push_str("</span><span class=\"pill\">channel ");
    esc_to(out, channel_label(comment.channel));
    out.push_str("</span><span class=\"pill\">replies ");
    esc_to(out, &comment.replies.len().to_string());
    out.push_str("</span>");
    out.push_str("<span class=\"loc\">");
    esc_to(out, &line_range(comment.line, comment.end_line));
    out.push_str("</span></div><p>");
    esc_to(out, &comment.body);
    out.push_str("</p><p class=\"provenance\"><strong>Observation:</strong> ");
    match &comment.observation {
        Some(observation) => {
            esc_to(
                out,
                short_fingerprint(&observation.snapshot.scope.aggregate),
            );
        }
        None => out.push_str("snapshot unavailable (legacy; target label is not proof)"),
    }
    if let Some(anchor) = &comment.anchor {
        out.push_str("</p><p class=\"provenance\"><strong>Anchor:</strong> ");
        esc_to(out, &anchor_summary(anchor));
        out.push_str("</p><p>");
    }
    for reply in &comment.replies {
        out.push_str("</p><p class=\"reply\"><strong>Reply by ");
        esc_to(
            out,
            &format!("{}:{}", identity_kind(reply.author.kind), reply.author.name),
        );
        out.push_str(":</strong> ");
        esc_to(out, &reply.body);
        out.push_str(" <span class=\"provenance\">");
        match &reply.result {
            Some(result) => {
                out.push_str("snapshot ");
                esc_to(out, short_fingerprint(&result.snapshot.scope.aggregate));
                out.push_str("; against observation: ");
                match result.observation_aggregate_fingerprint.as_deref() {
                    Some(fingerprint) => esc_to(out, short_fingerprint(fingerprint)),
                    None => out.push_str("unavailable (legacy comment)"),
                }
                out.push_str("; relation: ");
                match &result.related {
                    crate::provenance::RelatedTransition::SamePath { path } => {
                        out.push_str("same_path ");
                        esc_to(out, path);
                    }
                    crate::provenance::RelatedTransition::RenamedFrom { old_path, path } => {
                        out.push_str("renamed_from ");
                        esc_to(out, old_path);
                        out.push_str(" to ");
                        esc_to(out, path);
                    }
                    crate::provenance::RelatedTransition::NotInDiff { path } => {
                        out.push_str("not_in_diff");
                        if let Some(path) = path {
                            out.push(' ');
                            esc_to(out, path);
                        } else {
                            out.push_str(" (general comment)");
                        }
                    }
                }
                out.push_str("; portable patch changed: ");
                out.push_str(match result.portable_patch_changed {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "unknown",
                });
            }
            None => out.push_str("result snapshot unavailable (legacy)"),
        }
        out.push_str("</span>");
    }
    out.push_str("</p></article>");
}

fn anchor_summary(anchor: &crate::anchor::CommentAnchor) -> String {
    match anchor {
        crate::anchor::CommentAnchor::File { path, old_path, .. } => {
            format!(
                "path={path} old_path={}",
                old_path.as_deref().unwrap_or("null")
            )
        }
        crate::anchor::CommentAnchor::Line {
            path,
            old_path,
            side,
            old_line,
            new_line,
            ..
        } => format!(
            "path={path} old_path={} side={} old_line={:?} new_line={:?}",
            old_path.as_deref().unwrap_or("null"),
            side.label(),
            old_line,
            new_line
        ),
        crate::anchor::CommentAnchor::Range {
            path,
            old_path,
            start_line,
            end_line,
            ..
        } => {
            format!(
                "path={path} old_path={} range={start_line}-{end_line}",
                old_path.as_deref().unwrap_or("null")
            )
        }
    }
}

fn short_fingerprint(value: &str) -> &str {
    value.get(..12).unwrap_or(value)
}

fn render_target_link(out: &mut String, target: &ReviewTarget) {
    let Some(file) = &target.file else {
        return;
    };
    out.push_str("<a class=\"target\" data-step-target href=\"#");
    out.push_str(&file_anchor(file));
    out.push_str("\">");
    esc_to(out, file);
    let range = line_range(target.line, target.end_line);
    if !range.is_empty() {
        out.push(':');
        esc_to(out, &range);
    }
    if let Some(symbol) = &target.symbol {
        out.push_str(" — ");
        esc_to(out, symbol);
    }
    out.push_str("</a>");
}

fn line_range(line: Option<usize>, end_line: Option<usize>) -> String {
    match (line, end_line) {
        (Some(a), Some(b)) if a != b => format!("{a}-{b}"),
        (Some(a), _) => a.to_string(),
        _ => String::new(),
    }
}

fn file_anchor(path: &str) -> String {
    let mut anchor = String::from("file-");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() {
            anchor.push(b as char);
        } else {
            anchor.push('-');
        }
    }
    anchor
}

fn esc_to(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diff::{DiffLine, DiffLineKind, DiffSet, FileDiff, FileStatus, Hunk},
        jj::ReviewTarget as JjReviewTarget,
        state::{CommentState, Walkthrough, WalkthroughStep},
    };
    use chrono::Utc;

    fn fixture() -> (ReviewSession, ReviewState) {
        let diff = DiffSet {
            raw_header: vec![],
            files: vec![FileDiff {
                path: "src/lib.rs".into(),
                old_path: None,
                status: FileStatus::Modified,
                additions: 1,
                deletions: 1,
                raw: String::new(),
                fingerprint: "fp".into(),
                hunks: vec![Hunk {
                    old_start: 1,
                    old_len: 2,
                    new_start: 1,
                    new_len: 2,
                    header: "@@ -1,2 +1,2 @@".into(),
                    lines: vec![
                        DiffLine {
                            kind: DiffLineKind::Removed,
                            old_lineno: Some(1),
                            new_lineno: None,
                            text: "<script>alert(1)</script>".into(),
                        },
                        DiffLine {
                            kind: DiffLineKind::Added,
                            old_lineno: None,
                            new_lineno: Some(1),
                            text: "safe".into(),
                        },
                    ],
                }],
            }],
        };
        let mut state = ReviewState::default();
        state.comments.push(Comment {
            id: "c1".into(),
            path: Some("src/lib.rs".into()),
            line: Some(1),
            end_line: None,
            anchor: None,
            body: "comment body".into(),
            kind: None,
            action: None,
            state: CommentState::Draft,
            created_at: Utc::now(),
            ..Default::default()
        });
        state.sessions.push(crate::state::ReviewSession {
            id: "s1".into(),
            target: ReviewTarget {
                repo: Some("/repo".into()),
                base: Some("main".into()),
                revision: Some("@".into()),
                ..Default::default()
            },
            attention_regions: vec![crate::state::AttentionRegion {
                target: ReviewTarget {
                    file: Some("src/lib.rs".into()),
                    anchor: Some(crate::anchor::CommentAnchor::File {
                        path: "src/lib.rs".into(),
                        old_path: None,
                        diff_fingerprint: "fp".into(),
                    }),
                    ..Default::default()
                },
                salience: crate::state::Salience::Spotlight,
                rationale: Some("private attention".into()),
                source: crate::state::SalienceSource::Human,
            }],
            action_items: vec![crate::state::ActionItem {
                id: "action-1".into(),
                title: "Address comment".into(),
                comment_ids: vec!["c1".into()],
                ..Default::default()
            }],
            walkthroughs: vec![Walkthrough {
                id: "w1".into(),
                title: None,
                steps: vec![WalkthroughStep {
                    id: "st1".into(),
                    title: Some("Step title".into()),
                    why: Some("because".into()),
                    body: Some("read it".into()),
                    target: ReviewTarget {
                        file: Some("src/lib.rs".into()),
                        line: Some(1),
                        end_line: Some(2),
                        symbol: Some("thing".into()),
                        ..Default::default()
                    },
                    ..WalkthroughStep::default()
                }],
                ..Walkthrough::default()
            }],
            ..Default::default()
        });
        let session = ReviewSession::new(
            std::path::PathBuf::from("/repo"),
            JjReviewTarget {
                base: "main".into(),
                rev: "@".into(),
            },
            diff,
            state.clone(),
        );
        (session, state)
    }

    #[test]
    fn renders_self_contained_html_with_escaped_content() {
        let (session, state) = fixture();
        let html = render_html(&session, &state);
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("src/lib.rs"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("comment body"));
        assert!(html.contains("Step title"));
        assert!(html.contains("Attention assignments"));
        assert!(html.contains("private attention"));
        assert!(html.contains("Address comment"));
        assert!(html.contains("snapshot unavailable (legacy; target label is not proof)"));
        assert_eq!(html.matches("comment body").count(), 1);
    }

    #[test]
    fn live_and_static_share_representative_guide_dom_semantics() {
        let (session, state) = fixture();
        let view = crate::web_render::GuideView::from_session(&session);
        let html = render_html(&session, &state);
        let options = crate::web_render::RenderOptions::static_artifact();

        let overview = crate::web_render::render_overview(&view, &view.target, options);
        assert!(
            html.contains(&overview),
            "static shell must embed the shared overview verbatim"
        );
        for region in &view.projection.regions {
            let shared = crate::web_render::render_region(
                region,
                crate::web_render::RenderMode::Full,
                options,
            );
            assert!(
                html.contains(&shared),
                "static shell must embed shared region {} verbatim",
                region.id
            );
        }
        let live_regions = view
            .projection
            .regions
            .iter()
            .map(|region| {
                crate::web_render::render_region(
                    region,
                    crate::web_render::RenderMode::Guided,
                    crate::web_render::RenderOptions::live(
                        crate::web_render::RenderMode::Guided,
                        false,
                    ),
                )
            })
            .collect::<String>();
        for semantic in ["data-region=", "diff-row", "salience-"] {
            assert!(html.contains(semantic), "static guide lacks {semantic}");
            assert!(
                live_regions.contains(semantic),
                "live guide lacks {semantic}"
            );
        }
    }

    #[test]
    fn static_export_has_only_embedded_assets_and_local_navigation() {
        let (session, state) = fixture();
        let html = render_html(&session, &state);
        for forbidden in [
            "<link ",
            "<script src=",
            "EventSource(",
            "fetch(",
            "/actions/",
            "data-token=",
            "?token=",
            "safe-token",
        ] {
            assert!(
                !html.contains(forbidden),
                "static export leaked {forbidden}"
            );
        }
        for hook in [
            "id=\"mode-switch\"",
            "data-theme-toggle",
            "data-local-action=\"context-toggle\"",
            "data-guide-nav=\"next\"",
            "scrollIntoView",
            "COMPONENT_CSS",
        ] {
            if hook == "COMPONENT_CSS" {
                assert!(html.contains("content-visibility: auto"));
            } else {
                assert!(html.contains(hook), "missing static navigation hook {hook}");
            }
        }
    }

    #[test]
    fn html_concisely_renders_embedded_observation_and_reply_result() {
        let (session, mut state) = fixture();
        let snapshot = crate::provenance::SnapshotEvidence::capture(
            chrono::DateTime::UNIX_EPOCH,
            "s1",
            state.sessions[0].target.clone(),
            session.files.iter().map(|file| &file.diff),
        );
        let observation = crate::provenance::CommentObservation::new(
            snapshot.clone(),
            Some(crate::anchor::CommentAnchor::File {
                path: "src/lib.rs".into(),
                old_path: None,
                diff_fingerprint: session.files[0].fingerprint.clone(),
            }),
        );
        state.comments[0].observation = Some(observation.clone());
        state.comments[0].replies.push(crate::state::CommentReply {
            id: "reply".into(),
            body: "done".into(),
            author: crate::state::Identity::agent(),
            created_at: chrono::DateTime::UNIX_EPOCH,
            result: Some(crate::provenance::CommentReplyResult::compare(
                "c1",
                Some(&observation),
                Some("src/lib.rs"),
                snapshot,
            )),
        });

        let html = render_html(&session, &state);

        assert!(html.contains("<strong>Observation:</strong>"));
        assert!(html.contains("portable patch changed: no"));
        assert!(html.contains("done"));
    }

    #[test]
    fn html_renders_rename_not_in_diff_and_missing_a_b_language() {
        let (session, mut state) = fixture();
        let renamed = DiffSet::parse(
            "diff --git a/src/lib.rs b/src/new.rs\nsimilarity index 100%\nrename from src/lib.rs\nrename to src/new.rs",
        )
        .unwrap();
        let snapshot = crate::provenance::SnapshotEvidence::capture(
            chrono::DateTime::UNIX_EPOCH,
            "s1",
            state.sessions[0].target.clone(),
            renamed.files.iter(),
        );
        state.comments[0].replies.extend([
            crate::state::CommentReply {
                id: "rename".into(),
                body: "renamed".into(),
                author: crate::state::Identity::agent(),
                created_at: chrono::DateTime::UNIX_EPOCH,
                result: Some(crate::provenance::CommentReplyResult {
                    parent_comment_id: "c1".into(),
                    observation_aggregate_fingerprint: Some("abcdef1234567890".into()),
                    snapshot: snapshot.clone(),
                    related: crate::provenance::RelatedTransition::RenamedFrom {
                        old_path: "src/lib.rs".into(),
                        path: "src/new.rs".into(),
                    },
                    portable_patch_changed: None,
                }),
            },
            crate::state::CommentReply {
                id: "missing-a".into(),
                body: "gone".into(),
                author: crate::state::Identity::agent(),
                created_at: chrono::DateTime::UNIX_EPOCH,
                result: Some(crate::provenance::CommentReplyResult {
                    parent_comment_id: "c1".into(),
                    observation_aggregate_fingerprint: None,
                    snapshot,
                    related: crate::provenance::RelatedTransition::NotInDiff {
                        path: Some("src/lib.rs".into()),
                    },
                    portable_patch_changed: None,
                }),
            },
            crate::state::CommentReply {
                id: "missing-b".into(),
                body: "legacy reply".into(),
                author: crate::state::Identity::local_human(),
                created_at: chrono::DateTime::UNIX_EPOCH,
                result: None,
            },
        ]);

        let html = render_html(&session, &state);

        assert!(html.contains("against observation: abcdef123456"));
        assert!(html.contains("relation: renamed_from src/lib.rs to src/new.rs"));
        assert!(html.contains("against observation: unavailable (legacy comment)"));
        assert!(html.contains("relation: not_in_diff src/lib.rs"));
        assert!(html.contains("result snapshot unavailable (legacy)"));
        assert!(html.contains("snapshot unavailable (legacy; target label is not proof)"));
    }

    #[test]
    fn acceptance_html_renders_general_comments_and_scopes_session_comments_with_legacy_visible() {
        let (session, mut state) = fixture();
        state.comments.extend([
            Comment {
                id: "general".into(),
                path: None,
                session_id: Some("s1".into()),
                body: "general body".into(),
                state: CommentState::Todo,
                channel: Channel::Delegation,
                ..Default::default()
            },
            Comment {
                id: "matching".into(),
                path: Some("src/lib.rs".into()),
                session_id: Some("s1".into()),
                body: "matching body".into(),
                state: CommentState::Resolved,
                ..Default::default()
            },
            Comment {
                id: "foreign".into(),
                path: Some("src/lib.rs".into()),
                session_id: Some("other".into()),
                body: "foreign body".into(),
                state: CommentState::Todo,
                channel: Channel::Delegation,
                ..Default::default()
            },
        ]);

        let html = render_html(&session, &state);

        assert!(html.contains("<h2>General comments</h2>"));
        assert!(html.contains("general body"));
        assert!(html.contains("matching body"));
        assert!(html.contains("comment body"));
        assert!(!html.contains("foreign body"));
    }

    #[test]
    fn html_scopes_durable_action_items_and_walkthroughs_to_active_session() {
        let (session, mut state) = fixture();
        state.sessions.push(crate::state::ReviewSession {
            id: "other".into(),
            action_items: vec![crate::state::ActionItem {
                id: "foreign-action".into(),
                title: "Foreign action item".into(),
                ..Default::default()
            }],
            walkthroughs: vec![Walkthrough {
                id: "foreign-walk".into(),
                steps: vec![WalkthroughStep {
                    id: "foreign-step".into(),
                    title: Some("Foreign step".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        });

        let html = render_html(&session, &state);

        assert!(html.contains("Address comment"));
        assert!(html.contains("Step title"));
        assert!(!html.contains("Foreign action item"));
        assert!(!html.contains("Foreign step"));
    }

    #[test]
    fn html_active_session_requires_matching_repo_identity() {
        let (session, mut state) = fixture();
        let mut wrong = state.sessions[0].clone();
        wrong.id = "wrong-repo".into();
        wrong.target.repo = Some("/other-repo".into());
        wrong.action_items[0].title = "Wrong repo action".into();
        state.sessions.insert(0, wrong);

        let html = render_html(&session, &state);

        assert!(html.contains("Address comment"));
        assert!(!html.contains("Wrong repo action"));
    }

    #[test]
    fn html_nests_linked_comments_and_suppresses_standalone_duplicates() {
        let (session, mut state) = fixture();
        state.comments.push(Comment {
            id: "unlinked".into(),
            session_id: Some("s1".into()),
            path: Some("src/lib.rs".into()),
            body: "unlinked body".into(),
            state: CommentState::Todo,
            channel: Channel::Delegation,
            ..Default::default()
        });

        let html = render_html(&session, &state);

        assert!(html.contains("<h4>Evidence comments</h4>"));
        assert_eq!(html.matches("comment body").count(), 1);
        assert_eq!(html.matches("unlinked body").count(), 1);
    }

    #[test]
    fn team_html_uses_team_projection_without_private_task_suppression() {
        let (session, mut state) = fixture();
        state.sessions[0].disposition = Some(crate::state::ReviewDisposition::RequestChanges);
        state.comments[0].state = CommentState::Todo;
        state.comments[0].channel = Channel::Collaboration;
        state.comments[0].anchor =
            crate::anchor::comment_anchor_for_file_diff(&session.files[0].diff, Some(1), Some(1));
        state.comments[0].author = crate::state::Identity {
            kind: crate::state::AuthorKind::Human,
            name: "Teammate".into(),
        };
        state.comments[0].replies.push(crate::state::CommentReply {
            id: "reply".into(),
            body: "reply body".into(),
            author: crate::state::Identity {
                kind: crate::state::AuthorKind::Agent,
                name: "Bot".into(),
            },
            created_at: Utc::now(),
            result: None,
        });
        state.comments.push(Comment {
            id: "private".into(),
            path: Some("src/lib.rs".into()),
            body: "private draft".into(),
            state: CommentState::Draft,
            channel: Channel::Collaboration,
            created_at: Utc::now(),
            ..Default::default()
        });

        let html = render_html_with_profile(&session, &state, ArtifactProfile::Team);
        assert!(html.contains("comment body"));
        assert!(html.contains("request-changes"));
        assert!(html.contains("human:Teammate"));
        assert!(html.contains("agent:Bot"));
        assert!(html.contains("side=new"));
        assert!(!html.contains("anchor-metadata"));
        assert!(!html.contains("application/json"));
        assert!(!html.contains("line_fingerprint"));
        assert!(!html.contains("diff_fingerprint"));
        assert!(!html.contains("hunk_index"));
        assert!(!html.contains("private draft"));
        assert!(!html.contains("private attention"));
        assert!(!html.contains("Attention assignments"));
        assert!(!html.contains("Address comment"));
    }

    #[test]
    fn html_renders_action_item_outcome_disposition_and_external_tickets() {
        let (session, mut state) = fixture();
        let item = &mut state.sessions[0].action_items[0];
        item.status = crate::state::ActionItemStatus::Closed;
        item.disposition = Some(crate::state::ClosedDisposition::Deferred);
        item.outcome = Some("Moved out of the local review".into());
        item.closed_at = Some(chrono::DateTime::UNIX_EPOCH);
        item.external_tickets.push(crate::state::ExternalTicket {
            tracker: "linear".into(),
            reference: "GAN-42".into(),
            url: Some("https://example.test/GAN-42?<unsafe>".into()),
            created_at: chrono::DateTime::UNIX_EPOCH,
            updated_at: chrono::DateTime::UNIX_EPOCH,
        });

        let html = render_html(&session, &state);

        assert!(html.contains("<strong>Disposition:</strong> deferred"));
        assert!(html.contains("Moved out of the local review"));
        assert!(html.contains("1970-01-01T00:00:00+00:00"));
        assert!(html.contains("linear GAN-42"));
        assert!(html.contains("https://example.test/GAN-42?&lt;unsafe&gt;"));
        assert!(!html.contains("?<unsafe>"));
    }
}
