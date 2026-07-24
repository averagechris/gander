//! Pure HTML view model and renderer shared by the live and static web adapters.
//!
//! Attention resolution, folding, chapter placement, and annotation ownership
//! are deliberately absent here. Those decisions arrive in `ReadingProjection`.

use crate::{
    app::{
        DiffRowKind, ReadingAnnotation, ReadingAnnotationSource, ReadingProjection, ReadingRegion,
        ReadingRegionKind, ReviewSession,
    },
    config::ThemeConfig,
    state::{AuthorKind, Channel, CommentState, Salience},
    theme::{Rgb, ThemeKind, ThemeSlots},
};

pub const COMPONENT_CSS: &str = include_str!("web.css");
pub const PREPAINT_SCRIPT: &str = "(()=>{try{let m=localStorage.getItem('gander.colorScheme');if(m==='light'||m==='dark')document.documentElement.dataset.colorScheme=m;}catch(e){}})();";
pub const THEME_CONTROL_SCRIPT: &str = "(()=>{let k='gander.colorScheme',o=['system','light','dark'],q=matchMedia('(prefers-color-scheme: dark)'),b=document.querySelector('[data-theme-toggle]'),l=document.querySelector('[data-theme-label]');function g(){try{return localStorage.getItem(k)||'system'}catch(e){return 'system'}}function s(m){document.documentElement.dataset.colorScheme=(m==='light'||m==='dark')?m:'';if(l)l.textContent=m}function set(m){try{m==='system'?localStorage.removeItem(k):localStorage.setItem(k,m)}catch(e){}s(m)}if(b)b.addEventListener('click',()=>set(o[(o.indexOf(g())+1)%o.length]));q.addEventListener&&q.addEventListener('change',()=>{if(g()==='system')s('system')});s(g())})();";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Guided,
    Full,
}

#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    /// Emit controls that POST durable mutations. Static artifacts leave this
    /// false while retaining local-only navigation and disclosure controls.
    pub actions: bool,
    /// Populate full skim rows now. The live initial guided response leaves
    /// them for its guarded generation-checked fragment route.
    pub eager_full: bool,
    pub fragment: bool,
    pub show_private_progress: bool,
}

impl RenderOptions {
    pub fn live(mode: RenderMode, fragment: bool) -> Self {
        Self {
            actions: true,
            eager_full: mode == RenderMode::Full,
            fragment,
            show_private_progress: true,
        }
    }

    pub const fn static_artifact() -> Self {
        Self {
            actions: false,
            eager_full: true,
            fragment: false,
            show_private_progress: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GuideFile {
    pub path: String,
    pub additions: usize,
    pub deletions: usize,
    pub viewed: bool,
    pub generated: bool,
    pub region_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GuideView {
    pub projection: ReadingProjection,
    pub files: Vec<GuideFile>,
    pub generation: u64,
    pub target: String,
}

impl GuideView {
    pub fn from_session(session: &ReviewSession) -> Self {
        let projection = session.reading_projection();
        let files = session
            .files
            .iter()
            .map(|file| GuideFile {
                path: file.path.clone(),
                additions: file.additions,
                deletions: file.deletions,
                viewed: file.viewed || file.caught_up,
                generated: file.generated,
                region_id: projection.file_region_ids.get(&file.path).cloned(),
            })
            .collect();
        Self {
            projection,
            files,
            generation: session.stream_inputs_generation(),
            target: session.target.to_string(),
        }
    }

    pub fn region(&self, id: &str) -> Option<&ReadingRegion> {
        self.projection
            .regions
            .iter()
            .find(|region| region.id == id)
    }
}

pub fn render_overview(view: &GuideView, target: &str, options: RenderOptions) -> String {
    let projection = &view.projection;
    let mut out = String::from(
        "<section id=\"overview\" data-patch-id=\"overview\" class=\"attention-map\" aria-labelledby=\"attention-title\"><p class=\"eyebrow\">Attention map</p><h1 id=\"attention-title\">",
    );
    escape_to(&mut out, &projection.summary);
    out.push_str("</h1><p class=\"meta\">Target <code>");
    escape_to(&mut out, target);
    out.push_str("</code></p><div class=\"metrics\">");
    metric(
        &mut out,
        "Spotlights",
        projection.spotlight_count,
        "curated stops",
    );
    metric(
        &mut out,
        "Skim folds",
        projection.skim_count,
        &format!("{} files", projection.skim_files),
    );
    if options.show_private_progress {
        out.push_str(&render_coverage(view));
    }
    let file_detail = if options.show_private_progress {
        format!(
            "{} viewed",
            view.files.iter().filter(|file| file.viewed).count()
        )
    } else {
        "changed files".to_owned()
    };
    metric(&mut out, "Files", view.files.len(), &file_detail);
    out.push_str("</div>");
    if !projection.chapters.is_empty() {
        out.push_str(
            "<nav class=\"chapters\" aria-label=\"Review chapters\"><h2>Chapters</h2><ol>",
        );
        for (index, chapter) in projection.chapters.iter().enumerate() {
            out.push_str("<li><a href=\"#");
            if let Some(region) = projection.regions.iter().find(|region| matches!(&region.kind, ReadingRegionKind::Chapter(candidate) if candidate == chapter)) {
                escape_to(&mut out, &region.id);
            }
            out.push_str("\"><span>");
            out.push_str(&(index + 1).to_string());
            out.push_str("</span> ");
            escape_to(
                &mut out,
                chapter
                    .description
                    .lines()
                    .next()
                    .unwrap_or(&chapter.change_id),
            );
            out.push_str("</a></li>");
        }
        out.push_str("</ol></nav>");
    }
    if projection.has_walkthrough {
        out.push_str(
            "<div class=\"walkthrough-actions\"><button type=\"button\" data-guide-nav=\"prev\"",
        );
        if options.actions {
            out.push_str(" data-action=\"walkthrough-prev\"");
        }
        out.push_str(">Previous spotlight</button><button class=\"primary-action\" type=\"button\" data-guide-nav=\"next\"");
        if options.actions {
            out.push_str(" data-action=\"walkthrough-next\"");
        }
        out.push_str(">Next spotlight</button></div>");
    }
    out.push_str("</section>");
    out
}

fn metric(out: &mut String, label: &str, value: usize, detail: &str) {
    out.push_str("<div class=\"metric\"><span>");
    escape_to(out, label);
    out.push_str("</span><strong>");
    out.push_str(&value.to_string());
    out.push_str("</strong><small>");
    escape_to(out, detail);
    out.push_str("</small></div>");
}

pub fn render_coverage(view: &GuideView) -> String {
    format!(
        "<div id=\"coverage\" data-patch-id=\"coverage\" class=\"metric\"><span>Coverage</span><strong>{}</strong><small>of {} attention units</small></div>",
        view.projection.coverage.covered, view.projection.coverage.total
    )
}

pub fn render_footer(view: &GuideView) -> String {
    format!(
        "<footer id=\"footer\" data-patch-id=\"footer\" class=\"meta\">Generation {} · coverage {}/{}</footer>",
        view.generation, view.projection.coverage.covered, view.projection.coverage.total
    )
}

pub fn render_file_tree(view: &GuideView, options: RenderOptions) -> String {
    let mut out = String::from(
        "<aside id=\"file-tree\" data-patch-id=\"file-tree\" class=\"file-tree full-only\" aria-label=\"Files\"><h2>Files</h2><p class=\"meta\">Traditional review</p><ul>",
    );
    for file in &view.files {
        out.push_str("<li data-search=\"");
        escape_to(&mut out, &file.path.to_lowercase());
        out.push_str("\"><label>");
        if options.actions {
            out.push_str("<input type=\"checkbox\" data-action=\"file-viewed\" data-path=\"");
            escape_to(&mut out, &file.path);
            out.push_str("\" ");
            if file.viewed {
                out.push_str("checked ");
            }
            out.push_str("aria-label=\"Viewed: ");
            escape_to(&mut out, &file.path);
            out.push_str("\">");
        } else if options.show_private_progress {
            out.push_str(if file.viewed {
                "<span aria-label=\"viewed\">✓</span>"
            } else {
                "<span aria-hidden=\"true\">•</span>"
            });
        } else {
            out.push_str("<span aria-hidden=\"true\">•</span>");
        }
        out.push_str("<a href=\"#");
        if let Some(region) = &file.region_id {
            escape_to(&mut out, region);
        }
        out.push_str("\">");
        escape_to(&mut out, &file.path);
        out.push_str("</a></label><small>+");
        out.push_str(&file.additions.to_string());
        out.push_str(" −");
        out.push_str(&file.deletions.to_string());
        if file.generated {
            out.push_str(" · generated");
        }
        out.push_str("</small></li>");
    }
    out.push_str("</ul></aside>");
    out
}

pub fn render_region(region: &ReadingRegion, mode: RenderMode, options: RenderOptions) -> String {
    let mut out = String::new();
    out.push_str("<section id=\"");
    escape_to(&mut out, &region.id);
    out.push_str("\" class=\"region ");
    out.push_str(match region.kind {
        ReadingRegionKind::Chapter(_) => "chapter-region",
        ReadingRegionKind::File { .. } => "file-region",
        ReadingRegionKind::Skim(_) => "skim-region",
    });
    out.push_str("\" data-region=\"");
    escape_to(&mut out, &region.id);
    if matches!(region.kind, ReadingRegionKind::Skim(_)) {
        out.push_str("\" data-full-loaded=\"");
        out.push_str(if options.eager_full { "true" } else { "false" });
    }
    out.push_str("\" data-search=\"");
    escape_to(&mut out, &region_search_text(region, mode));
    out.push_str("\">");
    match &region.kind {
        ReadingRegionKind::Chapter(chapter) => {
            out.push_str("<header class=\"chapter\"><p class=\"eyebrow\">Chapter</p><h2>");
            escape_to(
                &mut out,
                chapter
                    .description
                    .lines()
                    .next()
                    .unwrap_or(&chapter.change_id),
            );
            out.push_str("</h2><p><code>");
            escape_to(&mut out, &chapter.change_id);
            out.push_str("</code>");
            if !chapter.bookmarks.is_empty() {
                out.push_str(" · ");
                escape_to(&mut out, &chapter.bookmarks);
            }
            out.push_str(" · +");
            out.push_str(&chapter.additions.to_string());
            out.push_str(" −");
            out.push_str(&chapter.deletions.to_string());
            out.push_str("</p></header>");
        }
        ReadingRegionKind::File { path, .. } => {
            out.push_str("<header class=\"file-header\"><h3>");
            escape_to(&mut out, path);
            out.push_str("</h3><div class=\"region-actions\">");
            if options.actions {
                for (label, action) in [
                    ("Promote", "salience-promote"),
                    ("Demote", "salience-demote"),
                ] {
                    out.push_str("<button type=\"button\" data-action=\"");
                    out.push_str(action);
                    out.push_str("\" data-path=\"");
                    escape_to(&mut out, path);
                    out.push_str("\">");
                    out.push_str(label);
                    out.push_str("</button>");
                }
                out.push_str("<button type=\"button\" data-action=\"salience-set\" data-salience=\"spotlight\" data-path=\"");
                escape_to(&mut out, path);
                out.push_str("\">Spotlight</button><button type=\"button\" data-action=\"salience-clear\" data-path=\"");
                escape_to(&mut out, path);
                out.push_str("\">Clear override</button>");
            }
            out.push_str("<button type=\"button\" data-local-action=\"context-toggle\" aria-expanded=\"true\">Collapse context</button></div></header><div class=\"diff-table\">");
            for row in &region.rows {
                render_diff_row(&mut out, row, options);
            }
            out.push_str("</div>");
        }
        ReadingRegionKind::Skim(fold) => {
            out.push_str("<div class=\"guided-only skim-fold\"><button type=\"button\" data-local-action=\"fold-toggle\" aria-expanded=\"false\">⌄</button><strong>");
            out.push_str(&fold.files.len().to_string());
            out.push_str(if fold.files.len() == 1 {
                " file"
            } else {
                " files"
            });
            out.push_str("</strong><span>");
            escape_to(&mut out, &fold.rationale);
            out.push_str("</span><small>+");
            out.push_str(&fold.additions.to_string());
            out.push_str(" −");
            out.push_str(&fold.deletions.to_string());
            if fold.acknowledged {
                out.push_str(" · ✓ acknowledged");
            }
            out.push_str("</small>");
            if options.actions {
                out.push_str(
                    "<button type=\"button\" data-action=\"skim-acknowledge\" data-fold-id=\"",
                );
                escape_to(&mut out, &fold.id);
                out.push('"');
                if fold.acknowledged {
                    out.push_str(" disabled");
                }
                out.push_str(">Acknowledge</button>");
            }
            out.push_str("</div><div class=\"full-only\">");
            if options.eager_full {
                out.push_str("<header class=\"file-header\"><h3>");
                escape_to(&mut out, &fold.files.join(", "));
                out.push_str("</h3><span class=\"salience-chip\">skim</span></header><div class=\"diff-table\">");
                for row in region.rows.iter().skip(1) {
                    render_diff_row(&mut out, row, options);
                }
                out.push_str("</div>");
            } else {
                out.push_str(
                    "<p class=\"fragment-loading\">Loading every line for full review…</p>",
                );
            }
            out.push_str("</div>");
            if let Some(row) = region.rows.first() {
                for annotation in &row.annotations {
                    render_annotation(&mut out, annotation, options);
                }
            }
        }
    }
    if options.fragment {
        out.push_str("<!-- gander-fragment -->");
    }
    out.push_str("</section>");
    out
}

fn render_diff_row(out: &mut String, row: &crate::app::ReadingRow, options: RenderOptions) {
    let Some(diff) = &row.diff else {
        for annotation in &row.annotations {
            render_annotation(out, annotation, options);
        }
        return;
    };
    let (kind, label) = match diff.kind {
        DiffRowKind::DiffLine(crate::diff::DiffLineKind::Added) => ("added", "+"),
        DiffRowKind::DiffLine(crate::diff::DiffLineKind::Removed) => ("removed", "−"),
        DiffRowKind::DiffLine(crate::diff::DiffLineKind::Context) => ("context", " "),
        DiffRowKind::DiffLine(crate::diff::DiffLineKind::Meta) => ("meta", "\\"),
        DiffRowKind::HunkHeader => ("hunk", "@@"),
        DiffRowKind::FileHeader => ("file-title", ""),
        DiffRowKind::Placeholder => ("placeholder", "…"),
        _ => ("structural", diff.prefix),
    };
    out.push_str("<div class=\"diff-row ");
    out.push_str(kind);
    if let Some(salience) = row.salience {
        out.push_str(" salience-");
        out.push_str(salience_label(salience));
    }
    out.push_str("\" id=\"");
    escape_to(out, &dom_row_id(&row.id));
    out.push_str("\" data-path=\"");
    escape_to(out, row.path.as_deref().unwrap_or_default());
    if let Some(crate::anchor::CommentAnchor::Line {
        side,
        old_line,
        new_line,
        hunk_header,
        ..
    }) = &row.anchor
    {
        out.push_str("\" data-side=\"");
        out.push_str(match side {
            crate::anchor::DiffSide::Old => "old",
            crate::anchor::DiffSide::New => "new",
        });
        if let Some(line) = old_line {
            out.push_str("\" data-old-line=\"");
            out.push_str(&line.to_string());
        }
        if let Some(line) = new_line {
            out.push_str("\" data-new-line=\"");
            out.push_str(&line.to_string());
        }
        out.push_str("\" data-hunk=\"");
        escape_to(out, hunk_header);
    }
    out.push_str("\"><span class=\"salience-margin\" aria-label=\"");
    escape_to(
        out,
        row.salience.map(salience_label).unwrap_or("structural"),
    );
    out.push_str("\"></span><span class=\"line-number old\">");
    if let Some(line) = diff.old_lineno {
        out.push_str(&line.to_string());
    }
    out.push_str("</span><span class=\"line-number new\">");
    if let Some(line) = diff.new_lineno {
        out.push_str(&line.to_string());
    }
    out.push_str("</span><span class=\"prefix\">");
    escape_to(out, label);
    out.push_str("</span><code>");
    escape_to(out, &diff.text);
    out.push_str("</code></div>");
    for annotation in &row.annotations {
        render_annotation(out, annotation, options);
    }
}

fn render_annotation(out: &mut String, annotation: &ReadingAnnotation, options: RenderOptions) {
    out.push_str("<article class=\"annotation channel-");
    out.push_str(channel_label(annotation.channel()));
    out.push_str("\" data-owner-row=\"");
    escape_to(out, &annotation.owner_row_id);
    out.push_str("\"><header><span class=\"channel\">→ ");
    escape_to(out, annotation.channel().audience_label());
    out.push_str("</span>");
    match &annotation.source {
        ReadingAnnotationSource::Comment(comment) => {
            out.push_str("<span hidden data-comment-id=\"");
            escape_to(out, &comment.id);
            out.push_str("\"></span><span>");
            escape_to(out, author_label(comment.author.kind));
            out.push(':');
            escape_to(out, &comment.author.name);
            out.push_str("</span><span class=\"badge\">");
            escape_to(out, comment.state.label());
            out.push_str("</span></header><h4>");
            escape_to(
                out,
                comment
                    .body
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("(empty comment)")
                    .trim(),
            );
            out.push_str("</h4><p>");
            escape_to(out, &comment.body);
            out.push_str("</p>");
            if let Some(anchor) = &comment.anchor {
                out.push_str("<p class=\"provenance\"><strong>Anchor:</strong> ");
                escape_to(out, &public_anchor_summary(anchor));
                out.push_str("</p>");
            }
            for reply in &comment.replies {
                out.push_str("<div class=\"reply\"><strong>");
                escape_to(out, author_label(reply.author.kind));
                out.push(':');
                escape_to(out, &reply.author.name);
                out.push_str("</strong><p>");
                escape_to(out, &reply.body);
                out.push_str("</p></div>");
            }
            if options.actions {
                out.push_str("<form class=\"reply-form\" data-comment-id=\"");
                escape_to(out, &comment.id);
                out.push_str("\"><textarea name=\"body\" required placeholder=\"Reply…\"></textarea><button type=\"submit\">Reply</button></form><div class=\"comment-actions\">");
                if comment.author.kind == AuthorKind::Agent
                    && comment.channel == Channel::Onboarding
                {
                    out.push_str("<button type=\"button\" data-action=\"comment-request\" data-comment-id=\"");
                    escape_to(out, &comment.id);
                    out.push_str("\">Ask agent about this</button>");
                }
                if comment.author.kind == AuthorKind::Agent
                    && comment.state == CommentState::Draft
                    && comment.channel == Channel::Onboarding
                {
                    out.push_str(
                        "<button type=\"button\" data-action=\"draft-accept\" data-comment-id=\"",
                    );
                    escape_to(out, &comment.id);
                    out.push_str("\">Accept</button><button type=\"button\" data-action=\"draft-discard\" data-comment-id=\"");
                    escape_to(out, &comment.id);
                    out.push_str("\">Discard</button>");
                } else {
                    out.push_str(
                        "<button type=\"button\" data-action=\"comment-edit\" data-comment-id=\"",
                    );
                    escape_to(out, &comment.id);
                    out.push_str("\" data-comment-body=\"");
                    escape_to(out, &comment.body);
                    out.push_str("\">Edit</button><button type=\"button\" data-action=\"comment-state\" data-state=\"");
                    out.push_str(if comment.state == CommentState::Resolved {
                        "todo"
                    } else {
                        "resolved"
                    });
                    out.push_str("\" data-comment-id=\"");
                    escape_to(out, &comment.id);
                    out.push_str("\">");
                    out.push_str(if comment.state == CommentState::Resolved {
                        "Reopen"
                    } else {
                        "Resolve"
                    });
                    out.push_str("</button>");
                }
                out.push_str("</div>");
            }
        }
        ReadingAnnotationSource::Walkthrough {
            step,
            target,
            part,
            rationale,
        } => {
            out.push_str("<span hidden data-step-id=\"");
            escape_to(out, &step.id);
            out.push_str("\" data-step-part=\"");
            out.push_str(&part.to_string());
            out.push_str("\"></span><span>");
            if let Some(author) = &step.author {
                escape_to(out, author_label(author.kind));
                out.push(':');
                escape_to(out, &author.name);
            } else {
                out.push_str("walkthrough");
            }
            out.push_str("</span><span class=\"badge\">spotlight ");
            out.push_str(&(part + 1).to_string());
            out.push_str("</span></header><h4>");
            escape_to(out, step.title.as_deref().unwrap_or("Walkthrough step"));
            out.push_str("</h4><p class=\"target\">");
            escape_to(out, &target_label(target));
            out.push_str("</p>");
            if let Some(why) = step.why.as_deref().filter(|value| !value.trim().is_empty()) {
                out.push_str("<p><strong>Why:</strong> ");
                escape_to(out, why);
                out.push_str("</p>");
            }
            if let Some(rationale) = rationale
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                out.push_str("<p><strong>Rationale:</strong> ");
                escape_to(out, rationale);
                out.push_str("</p>");
            }
            if let Some(body) = step
                .body
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                out.push_str("<p>");
                escape_to(out, body);
                out.push_str("</p>");
            }
            for artifact in &step.artifacts {
                out.push_str("<details><summary>");
                escape_to(out, &artifact.title);
                out.push_str("</summary><pre>");
                escape_to(out, &artifact.body);
                out.push_str("</pre></details>");
            }
        }
    }
    out.push_str("</article>");
}

pub fn region_label(region: &ReadingRegion) -> String {
    match &region.kind {
        ReadingRegionKind::Chapter(chapter) => format!("Chapter {}", chapter.change_id),
        ReadingRegionKind::File { path, .. } => path.clone(),
        ReadingRegionKind::Skim(fold) => format!("Skim: {}", fold.rationale),
    }
}
fn region_search_text(region: &ReadingRegion, mode: RenderMode) -> String {
    let mut text = region_label(region).to_lowercase();
    let rows = if mode == RenderMode::Guided && matches!(region.kind, ReadingRegionKind::Skim(_)) {
        &region.rows[..region.rows.len().min(1)]
    } else {
        &region.rows
    };
    for row in rows {
        if let Some(diff) = &row.diff {
            text.push(' ');
            text.push_str(&diff.text.to_lowercase());
        }
        for annotation in &row.annotations {
            match &annotation.source {
                ReadingAnnotationSource::Comment(comment) => {
                    text.push(' ');
                    text.push_str(&comment.body.to_lowercase());
                }
                ReadingAnnotationSource::Walkthrough { step, .. } => {
                    text.push(' ');
                    text.push_str(&step.title.as_deref().unwrap_or_default().to_lowercase());
                    text.push(' ');
                    text.push_str(&step.body.as_deref().unwrap_or_default().to_lowercase());
                }
            }
        }
    }
    text
}
fn target_label(target: &crate::state::ReviewTarget) -> String {
    let mut label = target
        .file
        .clone()
        .unwrap_or_else(|| "review target".into());
    if let Some(line) = target.line {
        label.push(':');
        label.push_str(&line.to_string());
        if let Some(end) = target.end_line.filter(|end| *end != line) {
            label.push('-');
            label.push_str(&end.to_string());
        }
    }
    label
}
fn public_anchor_summary(anchor: &crate::anchor::CommentAnchor) -> String {
    match anchor {
        crate::anchor::CommentAnchor::File { path, old_path, .. } => format!(
            "path={path} old_path={}",
            old_path.as_deref().unwrap_or("null")
        ),
        crate::anchor::CommentAnchor::Line {
            path,
            old_path,
            side,
            old_line,
            new_line,
            ..
        } => format!(
            "path={path} old_path={} side={} old_line={old_line:?} new_line={new_line:?}",
            old_path.as_deref().unwrap_or("null"),
            side.label()
        ),
        crate::anchor::CommentAnchor::Range {
            path,
            old_path,
            start_line,
            end_line,
            ..
        } => format!(
            "path={path} old_path={} range={start_line}-{end_line}",
            old_path.as_deref().unwrap_or("null")
        ),
    }
}
fn salience_label(salience: Salience) -> &'static str {
    match salience {
        Salience::Spotlight => "spotlight",
        Salience::Supporting => "supporting",
        Salience::Skim => "skim",
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
fn author_label(kind: AuthorKind) -> &'static str {
    match kind {
        AuthorKind::Human => "human",
        AuthorKind::Agent => "agent",
    }
}
pub fn dom_row_id(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"gander-web-row-v1\0");
    hash.update(value.as_bytes());
    format!("row-{:x}", hash.finalize())
}

pub fn escape_to(out: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(character),
        }
    }
}

pub fn render_theme_css(config: &ThemeConfig) -> String {
    let light_palette = config.base_palette(ThemeKind::Light);
    let dark_palette = config.base_palette(ThemeKind::Dark);
    let light = ThemeSlots::derive(light_palette, light_palette.background);
    let dark = ThemeSlots::derive(dark_palette, dark_palette.background);
    format!(
        ":root,[data-color-scheme=light]{{color-scheme:light;{}}}@media(prefers-color-scheme:dark){{:root{{color-scheme:dark;{}}}}}[data-color-scheme=dark]{{color-scheme:dark;{}}}",
        slot_tokens(light_palette.background, light),
        slot_tokens(dark_palette.background, dark),
        slot_tokens(dark_palette.background, dark)
    )
}
fn slot_tokens(background: Rgb, slots: ThemeSlots) -> String {
    format!(
        "--background:{};--foreground:{};--muted:{};--subtle:{};--accent:{};--warning:{};--info:{};--detail:{};--secondary:{};--positive:{};--negative:{};--surface:{};--range-bg:{};--added-line-bg:{};--removed-line-bg:{};--shadow:{};--added-word-fg:{};--added-word-bg:{};--removed-word-fg:{};--removed-word-bg:{};--gutter-added-fg:{};--gutter-removed-fg:{};",
        css_rgb(background),
        css_rgb(slots.foreground),
        css_rgb(slots.muted),
        css_rgb(slots.subtle),
        css_rgb(slots.accent),
        css_rgb(slots.warning),
        css_rgb(slots.info),
        css_rgb(slots.detail),
        css_rgb(slots.secondary),
        css_rgb(slots.positive),
        css_rgb(slots.negative),
        css_rgb(slots.selection_bg),
        css_rgb(slots.range_bg),
        css_rgb(slots.added_line_bg),
        css_rgb(slots.removed_line_bg),
        css_rgb(slots.range_bg),
        css_rgb(slots.added_word_fg),
        css_rgb(slots.added_word_bg),
        css_rgb(slots.removed_word_fg),
        css_rgb(slots.removed_word_bg),
        css_rgb(slots.gutter_added_fg),
        css_rgb(slots.gutter_removed_fg)
    )
}
fn css_rgb(rgb: Rgb) -> String {
    format!("rgb({} {} {})", rgb.r, rgb.g, rgb.b)
}
