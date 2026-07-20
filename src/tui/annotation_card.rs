//! Shared presentation model and terminal layout for inline annotations.
//!
//! This is deliberately a view model, not durable state. Comments (including
//! agent drafts) and walkthrough narration project into the same card without
//! introducing another persisted annotation type.

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static CARD_PROJECTIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn record_projection() {
    CARD_PROJECTIONS.with(|count| count.set(count.get() + 1));
}

use crate::state::{
    ActionIntent, AuthorKind, Channel, Comment, CommentKind, CommentState, Identity, StepArtifact,
    StepArtifactKind, WalkthroughStep,
};

use super::{text_layout::VisualTextLayout, theme::AppTheme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AnnotationCardDensity {
    Expanded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BadgeTone {
    Channel,
    Muted,
    Info,
    Secondary,
    Positive,
    Negative,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CardBadge {
    label: String,
    tone: BadgeTone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AnnotationArtifact {
    title: String,
    kind: String,
    body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AnnotationReply {
    author: Identity,
    body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum AnnotationSource {
    Comment { id: String },
    Walkthrough { step_id: String, part: usize },
}

impl AnnotationSource {
    pub(super) fn stable_id(&self) -> String {
        match self {
            Self::Comment { id } => format!("comment:{id}"),
            Self::Walkthrough { step_id, part } => format!("walkthrough:{step_id}:{part}"),
        }
    }

    pub(super) fn comment_id(&self) -> Option<&str> {
        match self {
            Self::Comment { id } => Some(id),
            Self::Walkthrough { .. } => None,
        }
    }

    pub(super) fn walkthrough_step_id(&self) -> Option<&str> {
        match self {
            Self::Walkthrough { step_id, .. } => Some(step_id),
            Self::Comment { .. } => None,
        }
    }
}

/// Lightweight owned projection consumed by layout caches and popup rows.
///
/// Observation payloads, timestamps, reply result evidence, and anchors are
/// intentionally absent. Geometry keeps only text that can actually paint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AnnotationCard {
    pub(super) source: AnnotationSource,
    pub(super) channel: Channel,
    author: Option<Identity>,
    badges: Vec<CardBadge>,
    title: String,
    body: Option<String>,
    why: Option<String>,
    rationale: Option<String>,
    target: String,
    replies: Vec<AnnotationReply>,
    artifacts: Vec<AnnotationArtifact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentTone {
    Border,
    Channel,
    Foreground,
    Muted,
    Subtle,
    Detail,
    Secondary,
    Info,
    Positive,
    Negative,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CardSegment {
    text: String,
    tone: SegmentTone,
    bold: bool,
    italic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CardLine {
    segments: Vec<CardSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AnnotationCardLayout {
    lines: Vec<CardLine>,
    pub(super) width: usize,
}

impl AnnotationCard {
    pub(super) fn from_comment(comment: &Comment) -> Self {
        #[cfg(test)]
        record_projection();
        let (title, body) = split_headline(&comment.body, "(empty comment)");
        let mut badges = vec![CardBadge {
            label: comment.state.label().to_owned(),
            tone: match comment.state {
                CommentState::Draft => BadgeTone::Muted,
                CommentState::Todo => BadgeTone::Negative,
                CommentState::Resolved => BadgeTone::Positive,
            },
        }];
        if let Some(kind) = comment.kind {
            badges.push(CardBadge {
                label: comment_kind_label(kind).to_owned(),
                tone: BadgeTone::Info,
            });
        }
        if let Some(action) = comment
            .action
            .filter(|action| *action != ActionIntent::None)
        {
            badges.push(CardBadge {
                label: action_intent_label(action).to_owned(),
                tone: BadgeTone::Secondary,
            });
        }
        if !comment.replies.is_empty() {
            badges.push(CardBadge {
                label: format!("{} replies", comment.replies.len()),
                tone: BadgeTone::Channel,
            });
        }
        Self {
            source: AnnotationSource::Comment {
                id: comment.id.clone(),
            },
            channel: comment.channel,
            author: Some(comment.author.clone()),
            badges,
            title,
            body,
            why: None,
            rationale: None,
            target: comment_location(comment),
            replies: comment
                .replies
                .iter()
                .map(|reply| AnnotationReply {
                    author: reply.author.clone(),
                    body: reply.body.clone(),
                })
                .collect(),
            artifacts: Vec::new(),
        }
    }

    pub(super) fn from_walkthrough_step(
        step: &WalkthroughStep,
        target: &crate::state::ReviewTarget,
        part: usize,
        rationale: Option<String>,
        load_artifact_bodies: bool,
    ) -> Self {
        #[cfg(test)]
        record_projection();
        let title = step
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or("Walkthrough step")
            .to_owned();
        let why = nonempty(step.why.clone());
        let rationale = nonempty(rationale).filter(|value| Some(value) != why.as_ref());
        Self {
            source: AnnotationSource::Walkthrough {
                step_id: step.id.clone(),
                part,
            },
            channel: Channel::Onboarding,
            author: step.author.clone(),
            badges: vec![
                CardBadge {
                    label: "walkthrough".to_owned(),
                    tone: BadgeTone::Channel,
                },
                CardBadge {
                    label: "spotlight".to_owned(),
                    tone: BadgeTone::Secondary,
                },
            ],
            title,
            body: nonempty(step.body.clone()),
            why,
            rationale,
            target: review_target_location(target),
            replies: Vec::new(),
            artifacts: step
                .artifacts
                .iter()
                .map(|artifact| AnnotationArtifact::from_step(artifact, load_artifact_bodies))
                .collect(),
        }
    }

    #[cfg(test)]
    pub(super) fn has_artifacts(&self) -> bool {
        !self.artifacts.is_empty()
    }

    /// Equality for measured geometry. Collapsed artifacts expose their title
    /// and kind but not their potentially large bodies, so body-only changes
    /// cannot churn the diff layout cache until the card is expanded.
    #[cfg(test)]
    pub(super) fn geometry_eq(&self, other: &Self, artifacts_expanded: bool) -> bool {
        self.source == other.source
            && self.channel == other.channel
            && self.author == other.author
            && self.badges == other.badges
            && self.title == other.title
            && self.body == other.body
            && self.why == other.why
            && self.rationale == other.rationale
            && self.target == other.target
            && self.replies == other.replies
            && self.artifacts.len() == other.artifacts.len()
            && self
                .artifacts
                .iter()
                .zip(&other.artifacts)
                .all(|(left, right)| {
                    left.title == right.title
                        && left.kind == right.kind
                        && (!artifacts_expanded || left.body == right.body)
                })
    }

    pub(super) fn layout(
        &self,
        width: usize,
        density: AnnotationCardDensity,
        artifacts_expanded: bool,
        artifact_key: &str,
    ) -> AnnotationCardLayout {
        let width = width.max(1);
        if width < 4 {
            let budget = width.saturating_sub(1);
            let mut compact = truncate_graphemes(&self.title, budget);
            compact.push_str(
                &" ".repeat(budget.saturating_sub(UnicodeWidthStr::width(compact.as_str()))),
            );
            return AnnotationCardLayout {
                lines: vec![CardLine {
                    segments: vec![
                        segment("▎", SegmentTone::Border),
                        segment(compact, SegmentTone::Channel),
                    ],
                }],
                width,
            };
        }

        let inner_width = width - 2;
        let mut lines = vec![border_line('╭', '─', '╮', width)];
        let mut metadata = self.author.as_ref().map_or_else(
            || "walkthrough".to_owned(),
            |author| format!("{}:{}", author_kind_label(author.kind), author.name),
        );
        metadata.push_str(&format!("  [→ {}]", self.channel.audience_label()));
        for badge in &self.badges {
            metadata.push_str(&format!("  [{}]", badge.label));
        }
        push_wrapped_box_line(
            &mut lines,
            &metadata,
            inner_width,
            SegmentTone::Muted,
            false,
            false,
        );
        push_wrapped_box_line(
            &mut lines,
            &self.title,
            inner_width,
            SegmentTone::Channel,
            true,
            false,
        );
        if !self.target.is_empty() {
            push_wrapped_box_line(
                &mut lines,
                &format!("@ {}", self.target),
                inner_width,
                SegmentTone::Detail,
                false,
                false,
            );
        }

        if density == AnnotationCardDensity::Expanded {
            if let Some(why) = &self.why {
                push_field(&mut lines, "why", why, inner_width, SegmentTone::Secondary);
            }
            if let Some(rationale) = &self.rationale {
                push_field(
                    &mut lines,
                    "rationale",
                    rationale,
                    inner_width,
                    SegmentTone::Secondary,
                );
            }
            if let Some(body) = &self.body {
                push_field(&mut lines, "body", body, inner_width, SegmentTone::Subtle);
            }
            for reply in &self.replies {
                let kind = match reply.author.kind {
                    AuthorKind::Human => "human",
                    AuthorKind::Agent => "agent",
                };
                push_field(
                    &mut lines,
                    &format!("reply · {kind}:{}", reply.author.name),
                    &reply.body,
                    inner_width,
                    SegmentTone::Subtle,
                );
            }
            if !self.artifacts.is_empty() {
                if artifacts_expanded {
                    push_wrapped_box_line(
                        &mut lines,
                        &format!("{artifact_key} collapse artifacts"),
                        inner_width,
                        SegmentTone::Muted,
                        false,
                        false,
                    );
                    for artifact in &self.artifacts {
                        push_field(
                            &mut lines,
                            &format!("{} · {}", artifact.kind, artifact.title),
                            artifact
                                .body
                                .as_deref()
                                .unwrap_or("(artifact body unavailable)"),
                            inner_width,
                            SegmentTone::Foreground,
                        );
                    }
                } else {
                    let titles = self
                        .artifacts
                        .iter()
                        .map(|artifact| artifact.title.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    push_wrapped_box_line(
                        &mut lines,
                        &format!(
                            "{artifact_key} expand {} artifact(s): {titles}",
                            self.artifacts.len()
                        ),
                        inner_width,
                        SegmentTone::Muted,
                        false,
                        false,
                    );
                }
            }
        }
        lines.push(border_line('╰', '─', '╯', width));
        AnnotationCardLayout { lines, width }
    }

    /// One-row popup/list presentation derived from the same projection.
    pub(super) fn compact_line(&self, selected: bool, theme: &AppTheme) -> Line<'static> {
        let mut spans = vec![
            Span::styled(
                format!("[→ {}] ", self.channel.audience_label()),
                Style::default().fg(theme.channel_color(self.channel)),
            ),
            Span::styled(
                self.author.as_ref().map_or_else(
                    || "walkthrough ".to_owned(),
                    |author| format!("{}:{} ", author_kind_label(author.kind), author.name),
                ),
                Style::default().fg(theme.muted),
            ),
        ];
        for badge in &self.badges {
            spans.push(Span::styled(
                format!("[{}] ", badge.label),
                badge_style(badge.tone, self.channel, theme),
            ));
        }
        if !self.target.is_empty() {
            spans.push(Span::styled(
                format!("{} ", self.target),
                Style::default().fg(theme.detail),
            ));
        }
        let mut style = Style::default().fg(theme.channel_color(self.channel));
        if selected {
            style = style.add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(self.title.clone(), style));
        Line::from(spans)
    }
}

#[cfg(test)]
pub(super) fn reset_projection_count() {
    CARD_PROJECTIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn projection_count() -> usize {
    CARD_PROJECTIONS.with(Cell::get)
}

impl AnnotationCardLayout {
    pub(super) fn len(&self) -> usize {
        self.lines.len()
    }

    #[cfg(test)]
    pub(super) fn plain_line(&self, index: usize) -> String {
        self.lines[index]
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect()
    }

    pub(super) fn line(
        &self,
        index: usize,
        channel: Channel,
        selected: bool,
        in_range: bool,
        theme: &AppTheme,
    ) -> Line<'static> {
        let background = if selected {
            Some(theme.selection_bg)
        } else if in_range {
            Some(theme.range_bg)
        } else {
            None
        };
        Line::from(
            self.lines[index]
                .segments
                .iter()
                .map(|segment| {
                    let mut style = segment_style(segment.tone, channel, theme);
                    if segment.bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if segment.italic {
                        style = style.add_modifier(Modifier::ITALIC);
                    }
                    if let Some(background) = background {
                        style = style.bg(background);
                    }
                    Span::styled(segment.text.clone(), style)
                })
                .collect::<Vec<_>>(),
        )
    }
}

impl AnnotationArtifact {
    fn from_step(artifact: &StepArtifact, load_body: bool) -> Self {
        Self {
            title: artifact.title.clone(),
            kind: match artifact.kind {
                StepArtifactKind::Example => "example",
                StepArtifactKind::Output => "output",
                StepArtifactKind::Diagram => "diagram",
                StepArtifactKind::Note => "note",
            }
            .to_owned(),
            body: load_body.then(|| artifact.body.clone()),
        }
    }
}

fn split_headline(body: &str, empty: &str) -> (String, Option<String>) {
    let mut found = None;
    for (index, line) in body.lines().enumerate() {
        if !line.trim().is_empty() {
            found = Some((index, line.trim().to_owned()));
            break;
        }
    }
    let Some((headline_index, headline)) = found else {
        return (empty.to_owned(), None);
    };
    let remainder = body
        .lines()
        .enumerate()
        .filter_map(|(index, line)| (index != headline_index).then_some(line))
        .collect::<Vec<_>>()
        .join("\n");
    (headline, nonempty(Some(remainder)))
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

fn comment_location(comment: &Comment) -> String {
    let Some(path) = comment.path.as_deref() else {
        return "general".to_owned();
    };
    match (comment.line, comment.end_line) {
        (Some(start), Some(end)) if end != start => format!("{path}:{start}-{end}"),
        (Some(line), _) => format!("{path}:{line}"),
        _ => path.to_owned(),
    }
}

pub(super) fn review_target_location(target: &crate::state::ReviewTarget) -> String {
    let Some(path) = target.file.as_deref() else {
        return "review target".to_owned();
    };
    match (target.line, target.end_line) {
        (Some(start), Some(end)) if end != start => format!("{path}:{start}-{end}"),
        (Some(line), _) => format!("{path}:{line}"),
        _ => path.to_owned(),
    }
}

fn push_field(
    lines: &mut Vec<CardLine>,
    label: &str,
    value: &str,
    width: usize,
    tone: SegmentTone,
) {
    push_wrapped_box_line(
        lines,
        &format!("{label}: {value}"),
        width,
        tone,
        false,
        label == "why" || label == "rationale",
    );
}

fn push_wrapped_box_line(
    lines: &mut Vec<CardLine>,
    text: &str,
    width: usize,
    tone: SegmentTone,
    bold: bool,
    italic: bool,
) {
    let layout = VisualTextLayout::read_only(text, width.max(1));
    for index in 0..layout.rows().len() {
        let text = layout.row_text(index);
        let padding = width.saturating_sub(UnicodeWidthStr::width(text));
        lines.push(CardLine {
            segments: vec![
                segment("│", SegmentTone::Border),
                CardSegment {
                    text: text.to_owned(),
                    tone,
                    bold,
                    italic,
                },
                segment(" ".repeat(padding), tone),
                segment("│", SegmentTone::Border),
            ],
        });
    }
}

fn border_line(left: char, fill: char, right: char, width: usize) -> CardLine {
    CardLine {
        segments: vec![CardSegment {
            text: format!(
                "{left}{}{right}",
                fill.to_string().repeat(width.saturating_sub(2))
            ),
            tone: SegmentTone::Border,
            bold: true,
            italic: false,
        }],
    }
}

fn segment(text: impl Into<String>, tone: SegmentTone) -> CardSegment {
    CardSegment {
        text: text.into(),
        tone,
        bold: false,
        italic: false,
    }
}

fn segment_style(tone: SegmentTone, channel: Channel, theme: &AppTheme) -> Style {
    Style::default().fg(match tone {
        SegmentTone::Border | SegmentTone::Channel => theme.channel_color(channel),
        SegmentTone::Foreground => theme.foreground,
        SegmentTone::Muted => theme.muted,
        SegmentTone::Subtle => theme.subtle,
        SegmentTone::Detail => theme.detail,
        SegmentTone::Secondary => theme.secondary,
        SegmentTone::Info => theme.info,
        SegmentTone::Positive => theme.positive,
        SegmentTone::Negative => theme.negative,
    })
}

fn badge_style(tone: BadgeTone, channel: Channel, theme: &AppTheme) -> Style {
    let segment = match tone {
        BadgeTone::Channel => SegmentTone::Channel,
        BadgeTone::Muted => SegmentTone::Muted,
        BadgeTone::Info => SegmentTone::Info,
        BadgeTone::Secondary => SegmentTone::Secondary,
        BadgeTone::Positive => SegmentTone::Positive,
        BadgeTone::Negative => SegmentTone::Negative,
    };
    segment_style(segment, channel, theme)
}

fn truncate_graphemes(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let suffix = if width > 1 { "…" } else { "" };
    let budget = width.saturating_sub(UnicodeWidthStr::width(suffix));
    let mut out = String::new();
    let mut used = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if used.saturating_add(grapheme_width) > budget {
            break;
        }
        out.push_str(grapheme);
        used += grapheme_width;
    }
    format!("{out}{suffix}")
}

fn author_kind_label(kind: AuthorKind) -> &'static str {
    match kind {
        AuthorKind::Human => "human",
        AuthorKind::Agent => "agent",
    }
}

fn action_intent_label(action: ActionIntent) -> &'static str {
    match action {
        ActionIntent::None => "none",
        ActionIntent::Fix => "fix",
        ActionIntent::Explain => "explain",
        ActionIntent::Test => "test",
        ActionIntent::FollowUp => "follow-up",
    }
}

fn comment_kind_label(kind: CommentKind) -> &'static str {
    match kind {
        CommentKind::Note => "note",
        CommentKind::Issue => "issue",
        CommentKind::Question => "question",
        CommentKind::Praise => "praise",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{CommentReply, ReviewTarget};

    #[test]
    fn comment_projection_excludes_heavy_evidence_and_keeps_visible_thread_data() {
        let comment = Comment {
            id: "c1".into(),
            path: Some("src/lib.rs".into()),
            line: Some(7),
            body: "Headline\nDetailed body".into(),
            state: CommentState::Todo,
            kind: Some(CommentKind::Question),
            action: Some(ActionIntent::Explain),
            author: Identity {
                kind: AuthorKind::Human,
                name: "Ada".into(),
            },
            channel: Channel::Delegation,
            replies: vec![CommentReply {
                author: Identity::agent(),
                body: "Because the invariant requires it.".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let card = AnnotationCard::from_comment(&comment);
        assert_eq!(card.title, "Headline");
        assert_eq!(card.body.as_deref(), Some("Detailed body"));
        assert_eq!(card.target, "src/lib.rs:7");
        assert_eq!(card.replies.len(), 1);
        assert!(card.badges.iter().any(|badge| badge.label == "1 replies"));
    }

    #[test]
    fn layout_is_exact_width_and_grapheme_safe_at_narrow_widths() {
        let card = AnnotationCard {
            source: AnnotationSource::Walkthrough {
                step_id: "unicode".into(),
                part: 0,
            },
            channel: Channel::Onboarding,
            author: Some(Identity::agent()),
            badges: Vec::new(),
            title: "界 e\u{301} 👩🏽‍💻 headline".into(),
            body: Some("body with family 👨‍👩‍👧‍👦".into()),
            why: None,
            rationale: None,
            target: "src/界.rs:1".into(),
            replies: Vec::new(),
            artifacts: Vec::new(),
        };
        for width in 1..24 {
            let layout = card.layout(width, AnnotationCardDensity::Expanded, false, "E");
            assert!(
                layout
                    .lines
                    .iter()
                    .all(|line| UnicodeWidthStr::width(layout_line_text(line).as_str()) == width),
                "width {width}"
            );
        }
    }

    #[test]
    fn walkthrough_projection_keeps_title_why_rationale_and_artifacts() {
        let step = WalkthroughStep {
            id: "s1".into(),
            target: ReviewTarget {
                file: Some("src/lib.rs".into()),
                line: Some(4),
                ..Default::default()
            },
            title: Some("Start here".into()),
            why: Some("Defines the invariant".into()),
            body: Some("Follow the value into the parser.".into()),
            artifacts: vec![StepArtifact {
                title: "call flow".into(),
                kind: StepArtifactKind::Diagram,
                body: "input -> parse -> output".into(),
            }],
            ..Default::default()
        };
        let card = AnnotationCard::from_walkthrough_step(
            &step,
            &step.target,
            0,
            Some("Highest mental-model delta".into()),
            true,
        );
        assert_eq!(card.why.as_deref(), Some("Defines the invariant"));
        assert_eq!(
            card.rationale.as_deref(),
            Some("Highest mental-model delta")
        );
        assert!(card.has_artifacts());
        let collapsed = card.layout(48, AnnotationCardDensity::Expanded, false, "E");
        let expanded = card.layout(48, AnnotationCardDensity::Expanded, true, "E");
        assert!(expanded.len() > collapsed.len());
        assert!(
            (0..expanded.len())
                .map(|index| expanded.plain_line(index))
                .any(|line| line.contains("input -> parse"))
        );
    }

    #[test]
    fn collapsed_geometry_ignores_hidden_artifact_bodies() {
        let step = WalkthroughStep {
            id: "s1".into(),
            artifacts: vec![StepArtifact {
                title: "flow".into(),
                kind: StepArtifactKind::Diagram,
                body: "before".into(),
            }],
            ..Default::default()
        };
        let before = AnnotationCard::from_walkthrough_step(&step, &step.target, 0, None, true);
        let mut changed = step;
        changed.artifacts[0].body = "after with much more content".into();
        let after = AnnotationCard::from_walkthrough_step(&changed, &changed.target, 0, None, true);
        assert!(before.geometry_eq(&after, false));
        assert!(!before.geometry_eq(&after, true));

        let deferred =
            AnnotationCard::from_walkthrough_step(&changed, &changed.target, 0, None, false);
        assert!(deferred.artifacts[0].body.is_none());
        assert!(after.artifacts[0].body.is_some());
    }

    #[test]
    fn snapshot_channels_authors_lifecycle_badges_and_replies() {
        let cases = [
            (
                Channel::Onboarding,
                CommentState::Draft,
                Identity::agent(),
                "Agent draft explains the entry point",
            ),
            (
                Channel::Delegation,
                CommentState::Todo,
                Identity {
                    kind: AuthorKind::Human,
                    name: "Ada".into(),
                },
                "Please fix the retry boundary",
            ),
            (
                Channel::Collaboration,
                CommentState::Resolved,
                Identity {
                    kind: AuthorKind::Human,
                    name: "Grace".into(),
                },
                "Team review thread is resolved",
            ),
            (
                Channel::Note,
                CommentState::Draft,
                Identity {
                    kind: AuthorKind::Human,
                    name: "Lin".into(),
                },
                "Private note for later",
            ),
        ];
        let mut output = String::new();
        for (index, (channel, state, author, body)) in cases.into_iter().enumerate() {
            let comment = Comment {
                id: format!("c{index}"),
                path: Some("src/retry.rs".into()),
                line: Some(index + 1),
                body: format!("{body}\nSecond line with context."),
                kind: Some(CommentKind::Question),
                action: Some(ActionIntent::Fix),
                state,
                author,
                channel,
                replies: (index == 1)
                    .then(|| CommentReply {
                        author: Identity::agent(),
                        body: "Fixed and covered by the boundary test.".into(),
                        ..Default::default()
                    })
                    .into_iter()
                    .collect(),
                ..Default::default()
            };
            let card = AnnotationCard::from_comment(&comment);
            output.push_str(&format!("\n-- {:?} --\n", channel));
            output.push_str(&plain_layout(&card.layout(
                64,
                AnnotationCardDensity::Expanded,
                false,
                "E",
            )));
        }
        insta::assert_snapshot!("annotation_cards_channels_authors_lifecycle", output);
    }

    #[test]
    fn snapshot_walkthrough_wrapping_and_expanded_artifacts() {
        let step = WalkthroughStep {
            id: "spotlight".into(),
            target: ReviewTarget {
                file: Some("src/parser/stream.rs".into()),
                line: Some(40),
                end_line: Some(48),
                ..Default::default()
            },
            title: Some("The parser now commits only after validation".into()),
            why: Some("This changes the failure boundary and the caller's mental model.".into()),
            body: Some(
                "Read validation first, then follow the commit into the buffered stream.".into(),
            ),
            artifacts: vec![
                StepArtifact {
                    title: "usage".into(),
                    kind: StepArtifactKind::Example,
                    body: "let parsed = stream.validate()?.commit();".into(),
                },
                StepArtifact {
                    title: "flow".into(),
                    kind: StepArtifactKind::Diagram,
                    body: "input -> validate -> commit\n          \\-> reject".into(),
                },
            ],
            ..Default::default()
        };
        let card = AnnotationCard::from_walkthrough_step(
            &step,
            &step.target,
            0,
            Some("Highest mental-model delta in this change.".into()),
            true,
        );
        let output = format!(
            "NARROW COLLAPSED\n{}\nWIDE COLLAPSED\n{}\nWIDE EXPANDED\n{}",
            plain_layout(&card.layout(36, AnnotationCardDensity::Expanded, false, "E")),
            plain_layout(&card.layout(76, AnnotationCardDensity::Expanded, false, "E")),
            plain_layout(&card.layout(76, AnnotationCardDensity::Expanded, true, "E")),
        );
        insta::assert_snapshot!("annotation_card_walkthrough_artifacts_wrapping", output);
    }

    #[test]
    fn snapshot_walkthrough_human_agent_and_neutral_attribution() {
        let base = WalkthroughStep {
            id: "attribution".into(),
            title: Some("Understand the boundary".into()),
            target: ReviewTarget {
                file: Some("src/lib.rs".into()),
                line: Some(9),
                ..Default::default()
            },
            ..Default::default()
        };
        let cases = [
            (
                "agent",
                Some(Identity {
                    kind: AuthorKind::Agent,
                    name: "review-agent".into(),
                }),
            ),
            (
                "human",
                Some(Identity {
                    kind: AuthorKind::Human,
                    name: "Ada".into(),
                }),
            ),
            ("neutral", None),
        ];
        let mut output = String::new();
        for (label, author) in cases {
            let mut step = base.clone();
            step.author = author;
            let card = AnnotationCard::from_walkthrough_step(&step, &step.target, 0, None, false);
            output.push_str(&format!("-- {label} --\n"));
            output.push_str(&plain_layout(&card.layout(
                58,
                AnnotationCardDensity::Expanded,
                false,
                "E",
            )));
        }
        insta::assert_snapshot!("annotation_card_walkthrough_attribution", output);
    }

    fn plain_layout(layout: &AnnotationCardLayout) -> String {
        let mut output = (0..layout.len())
            .map(|index| layout.plain_line(index))
            .collect::<Vec<_>>()
            .join("\n");
        output.push('\n');
        output
    }

    fn layout_line_text(line: &CardLine) -> String {
        line.segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect()
    }
}
