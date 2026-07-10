use chrono::Utc;

use crate::{
    app::ReviewSession,
    diff::DiffLineKind,
    state::{Comment, ReviewState, ReviewTarget},
};

pub fn render_html(session: &ReviewSession, state: &ReviewState) -> String {
    let generated_at = Utc::now().to_rfc3339();
    let mut out = String::from(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>gander review export</title>\n<style>",
    );
    out.push_str(CSS);
    out.push_str("</style>\n</head>\n<body>\n");
    out.push_str("<header class=\"hero\"><div><p class=\"eyebrow\">gander review export</p><h1>");
    esc_to(&mut out, &session.repo.display().to_string());
    out.push_str("</h1><p class=\"summary\">");
    esc_to(&mut out, &session.summary_line());
    out.push_str("</p></div><div class=\"meta\"><div><span>Target</span><b>");
    esc_to(&mut out, &session.target.base);
    out.push_str(" → ");
    esc_to(&mut out, &session.target.rev);
    out.push_str("</b></div><div><span>Generated</span><b>");
    esc_to(&mut out, &generated_at);
    out.push_str(
        "</b></div></div></header>\n<div class=\"layout\"><nav class=\"sidebar\"><h2>Files</h2>",
    );
    for file in &session.files {
        out.push_str("<a class=\"file-link\" href=\"#");
        out.push_str(&file_anchor(&file.path));
        out.push_str("\"><span>");
        out.push_str(if file.viewed {
            "✓ "
        } else if file.caught_up {
            "◌ "
        } else {
            "• "
        });
        esc_to(&mut out, &file.path);
        out.push_str("</span><small><ins>+");
        out.push_str(&file.additions.to_string());
        out.push_str("</ins> <del>-");
        out.push_str(&file.deletions.to_string());
        out.push_str("</del>");
        if file.generated {
            out.push_str(" <em>generated</em>");
        }
        out.push_str("</small></a>");
    }
    out.push_str("</nav><main>\n");
    render_walkthroughs(&mut out, state);
    render_tasks(&mut out, state);
    for file in &session.files {
        let comments: Vec<_> = state
            .comments
            .iter()
            .filter(|comment| comment.path == file.path)
            .collect();
        out.push_str("<section class=\"card file\" id=\"");
        out.push_str(&file_anchor(&file.path));
        out.push_str("\"><details open><summary><h2>");
        esc_to(&mut out, &file.path);
        out.push_str("</h2><span class=\"pill\">");
        esc_to(&mut out, &file.status.to_string());
        out.push_str("</span><span class=\"stat add\">+");
        out.push_str(&file.additions.to_string());
        out.push_str("</span><span class=\"stat del\">-");
        out.push_str(&file.deletions.to_string());
        out.push_str("</span></summary><table class=\"diff\"><tbody>");
        for hunk in &file.diff.hunks {
            out.push_str("<tr class=\"hunk\"><td colspan=\"3\">");
            esc_to(&mut out, &hunk.header);
            out.push_str("</td></tr>");
            for line in &hunk.lines {
                let class = match line.kind {
                    DiffLineKind::Added => "add",
                    DiffLineKind::Removed => "del",
                    DiffLineKind::Meta => "meta",
                    DiffLineKind::Context => "ctx",
                };
                out.push_str("<tr class=\"");
                out.push_str(class);
                out.push_str("\"><td class=\"ln\">");
                line_no(&mut out, line.old_lineno);
                out.push_str("</td><td class=\"ln\">");
                line_no(&mut out, line.new_lineno);
                out.push_str("</td><td><pre>");
                esc_to(&mut out, &line.text);
                out.push_str("</pre></td></tr>");
            }
        }
        out.push_str("</tbody></table>");
        if !comments.is_empty() {
            out.push_str("<div class=\"comments\"><h3>Comments</h3>");
            for comment in comments {
                render_comment(&mut out, comment);
            }
            out.push_str("</div>");
        }
        out.push_str("</details></section>\n");
    }
    out.push_str("</main></div><script>document.querySelectorAll('[data-step-target]').forEach(a=>a.addEventListener('click',()=>{const e=document.querySelector(a.getAttribute('href')); if(e) e.querySelector('details')?.setAttribute('open','');}));</script>\n</body></html>\n");
    out
}

fn render_walkthroughs(out: &mut String, state: &ReviewState) {
    let steps: Vec<_> = state
        .sessions
        .iter()
        .flat_map(|session| session.walkthroughs.iter())
        .flat_map(|walkthrough| walkthrough.steps.iter())
        .collect();
    if steps.is_empty() {
        return;
    }
    out.push_str("<section class=\"card\"><h2>Walkthrough</h2><ol class=\"walkthrough\">");
    for step in steps {
        out.push_str("<li><h3>");
        esc_to(out, step.title.as_deref().unwrap_or("Review step"));
        out.push_str("</h3>");
        if let Some(why) = &step.why {
            out.push_str("<p class=\"why\">Why: ");
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

fn render_tasks(out: &mut String, state: &ReviewState) {
    let tasks: Vec<_> = state
        .sessions
        .iter()
        .flat_map(|session| session.tasks.iter())
        .collect();
    if tasks.is_empty() {
        return;
    }
    out.push_str("<section class=\"card\"><h2>Tasks</h2>");
    for task in tasks {
        out.push_str("<article class=\"task\"><div><span class=\"pill\">");
        esc_to(out, &format!("{:?}", task.status).to_lowercase());
        out.push_str("</span><span class=\"pill action\">");
        esc_to(out, &format!("{:?}", task.action).to_lowercase());
        out.push_str("</span></div><h3>");
        esc_to(out, &task.title);
        out.push_str("</h3>");
        if let Some(body) = &task.body {
            out.push_str("<p>");
            esc_to(out, body);
            out.push_str("</p>");
        }
        if let Some(target) = &task.target {
            render_target_link(out, target);
        }
        out.push_str("</article>");
    }
    out.push_str("</section>");
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
    out.push_str("</span><span class=\"pill\">replies ");
    esc_to(out, &comment.replies.len().to_string());
    out.push_str("</span>");
    out.push_str("<span class=\"loc\">");
    esc_to(out, &line_range(comment.line, comment.end_line));
    out.push_str("</span></div><p>");
    esc_to(out, &comment.body);
    for reply in &comment.replies {
        out.push_str("</p><p class=\"reply\"><strong>Reply:</strong> ");
        esc_to(out, &reply.body);
    }
    out.push_str("</p></article>");
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

fn line_no(out: &mut String, line: Option<usize>) {
    if let Some(line) = line {
        out.push_str(&line.to_string());
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

const CSS: &str = r#"
:root{color-scheme:dark;--bg:#0f1117;--panel:#191724;--panel2:#1f1d2e;--text:#e6e1e8;--muted:#908caa;--rose:#eb6f92;--iris:#c4a7e7;--green:#3fb950;--red:#f85149;--line:#2a2837}*{box-sizing:border-box}body{margin:0;background:radial-gradient(circle at top left,#26233a,#0f1117 34rem);color:var(--text);font:14px/1.5 ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}.hero{display:flex;justify-content:space-between;gap:2rem;padding:2rem 2.4rem;border-bottom:1px solid var(--line);background:rgba(15,17,23,.82);backdrop-filter:blur(10px)}h1,h2,h3,p{margin-top:0}.eyebrow{color:var(--rose);font-weight:700;text-transform:uppercase;letter-spacing:.12em}.summary{color:var(--muted);font-size:1.05rem}.meta{display:grid;gap:.7rem;min-width:18rem}.meta div,.card{border:1px solid var(--line);background:rgba(25,23,36,.88);border-radius:16px}.meta div{padding:.8rem 1rem}.meta span{display:block;color:var(--muted);font-size:.78rem}.layout{display:grid;grid-template-columns:18rem 1fr;gap:1.2rem;padding:1.2rem}.sidebar{position:sticky;top:1rem;align-self:start;padding:1rem;border:1px solid var(--line);border-radius:16px;background:rgba(25,23,36,.92)}.file-link{display:block;padding:.55rem .2rem;color:var(--text);text-decoration:none;border-top:1px solid #242133}.file-link small{display:block;color:var(--muted)}ins{color:var(--green);text-decoration:none}del{color:var(--red);text-decoration:none}em,.pill{display:inline-block;border:1px solid var(--line);border-radius:999px;padding:.12rem .5rem;color:var(--iris);font-style:normal;font-size:.75rem}.card{margin-bottom:1rem;padding:1rem;box-shadow:0 16px 40px rgba(0,0,0,.28)}summary{cursor:pointer;display:flex;align-items:center;gap:.6rem}summary h2{display:inline;margin:0;flex:1}.stat{font-weight:700}.add{color:var(--green)}.del{color:var(--red)}.diff{width:100%;border-collapse:collapse;margin-top:1rem;overflow:hidden;border-radius:12px}.diff td{border-top:1px solid #242133}.ln{width:4.2rem;text-align:right;color:var(--muted);user-select:none;padding:.08rem .7rem;background:#15131f}.diff pre{margin:0;white-space:pre-wrap;font:12.5px/1.55 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}.diff .add td{background:rgba(46,160,67,.13)}.diff .del td{background:rgba(248,81,73,.13)}.diff .hunk td{padding:.42rem .8rem;color:var(--iris);background:#211f30;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}.comment,.task{padding:1rem;margin:.75rem 0;border:1px solid var(--line);border-radius:12px;background:var(--panel2)}.action{color:var(--rose)}.loc,.target{color:var(--muted);margin-left:.4rem}.target{display:inline-block;margin:.4rem 0 0 0}.why{color:#f6c177}@media(max-width:900px){.layout{display:block}.sidebar{position:static;margin-bottom:1rem}.hero{display:block}}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diff::{DiffLine, DiffSet, FileDiff, FileStatus, Hunk},
        jj::ReviewTarget as JjReviewTarget,
        state::{CommentState, Walkthrough, WalkthroughStep},
    };

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
            path: "src/lib.rs".into(),
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
    }
}
