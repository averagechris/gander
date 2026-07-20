//! Durable attention-map domain service.
//!
//! Source precedence is global (human > agent > heuristic). Within one source,
//! a hunk/range assignment is more specific than a file assignment; overlapping
//! ranges prefer the narrowest range. Remaining malformed duplicate ties prefer
//! higher salience, then canonical target/rationale order. Stale assignments are
//! retained for re-anchoring but do not affect the current diff.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use color_eyre::eyre::{Result, eyre};
use globset::{Glob, GlobSetBuilder};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    anchor::{CommentAnchor, comment_anchor_for_file_diff},
    diff::FileDiff,
    generated::{GeneratedMatcher, GeneratedPolicy, GeneratedPreset, diff_content_looks_generated},
    state::{
        AttentionProgress, AttentionProgressKind, AttentionProgressMember, AttentionProgressTarget,
        AttentionRegion, ReviewSession, ReviewState, ReviewTarget, Salience, SalienceSource,
        StepImportance, StepKind,
    },
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RegionKey {
    file: String,
    line: Option<usize>,
    end_line: Option<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AssignedAttentionRegion {
    pub target: ReviewTarget,
    pub salience: Salience,
    pub rationale: Option<String>,
    pub source: SalienceSource,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EffectiveAttentionRegion {
    pub target: ReviewTarget,
    pub salience: Salience,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SalienceSource>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct HeuristicUpdate {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub preserved_stale: usize,
}

/// One row on the current attention glance board. Current rows are the same
/// stable, fingerprint-guarded skim identities used by the review stream;
/// stale rows retain durable assignment history but can never be acknowledged.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SkimFoldSummary {
    pub id: String,
    pub target: AttentionProgressTarget,
    pub paths: Vec<String>,
    pub file_count: usize,
    pub additions: usize,
    pub deletions: usize,
    pub rationale: String,
    pub current: bool,
    pub stale: bool,
    pub acknowledged: bool,
    pub whole_files: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct AttentionCoverage {
    pub covered: usize,
    pub total: usize,
    pub skim_acknowledged: usize,
    pub skim_total: usize,
    pub spotlight_visited: usize,
    pub spotlight_total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkimSelection {
    AllCurrent,
    StableId(String),
    Target {
        path: String,
        line: Option<usize>,
        end_line: Option<usize>,
    },
}

impl SkimSelection {
    fn validate(&self) -> Result<()> {
        let Self::Target {
            path,
            line,
            end_line,
        } = self
        else {
            return Ok(());
        };
        if path.trim().is_empty() {
            return Err(eyre!("skim target path must not be empty"));
        }
        match (*line, *end_line) {
            (None, None) => Ok(()),
            (None, Some(_)) => Err(eyre!("skim target end line requires a start line")),
            (Some(0), _) | (_, Some(0)) => Err(eyre!("skim target line numbers are 1-indexed")),
            (Some(start), Some(end)) if end < start => Err(eyre!(
                "skim target end line must be greater than or equal to its start line"
            )),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SkimAcknowledgeOutcome {
    pub matched: usize,
    pub acknowledged: usize,
    pub already_acknowledged: usize,
    pub stale: usize,
    pub whole_files_viewed: Vec<String>,
}

#[derive(Debug)]
struct PendingSkimFold {
    rationale: String,
    targets: Vec<ReviewTarget>,
    paths: BTreeSet<String>,
    additions: usize,
    deletions: usize,
    whole_files: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FoldSignature {
    salience: Salience,
    rationale: Option<String>,
    source: Option<SalienceSource>,
}

fn fold_signature(region: &EffectiveAttentionRegion) -> FoldSignature {
    FoldSignature {
        salience: region.salience,
        rationale: region.rationale.clone(),
        source: region.source,
    }
}

/// Canonical stable identity shared by the stream, glance popup, CLI, and MCP.
pub fn skim_fold_id(target: &AttentionProgressTarget, rationale: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"gander-skim-fold-v1\0");
    hash.update(serde_json::to_vec(target).unwrap_or_default());
    hash.update(rationale.as_bytes());
    format!("fold:{:x}", hash.finalize())
}

fn stale_skim_fold_id(region: &AttentionRegion) -> String {
    let mut hash = Sha256::new();
    hash.update(b"gander-stale-skim-fold-v1\0");
    hash.update([source_rank(region.source)]);
    hash.update(serde_json::to_vec(&region.target).unwrap_or_default());
    format!("stale:{:x}", hash.finalize())
}

pub fn skim_rationale(rationale: Option<String>) -> String {
    let value = rationale.unwrap_or_else(|| "skimmed change".to_owned());
    let value = value.trim().trim_start_matches("Skim:").trim();
    let lowercase = value.to_ascii_lowercase();
    if lowercase.contains("generated") {
        "generated churn".to_owned()
    } else if lowercase.contains("lockfile") {
        "lockfile churn".to_owned()
    } else if lowercase.contains("ignore") {
        "policy-matched churn".to_owned()
    } else if value.is_empty() {
        "skimmed change".to_owned()
    } else {
        value.to_owned()
    }
}

fn flush_skim_summary(
    pending: &mut Option<PendingSkimFold>,
    output: &mut Vec<SkimFoldSummary>,
    session: &ReviewSession,
    files: &[FileDiff],
) {
    let Some(pending) = pending.take() else {
        return;
    };
    let target = AttentionProgressTarget::from_targets(pending.targets.iter());
    let refs = files.iter().collect::<Vec<_>>();
    let acknowledged = attention_progress_is_current_refs(
        session,
        &target,
        AttentionProgressKind::SkimAcknowledged,
        &refs,
    );
    let paths = pending.paths.into_iter().collect::<Vec<_>>();
    output.push(SkimFoldSummary {
        id: skim_fold_id(&target, &pending.rationale),
        target,
        file_count: paths.len(),
        paths,
        additions: pending.additions,
        deletions: pending.deletions,
        rationale: pending.rationale,
        current: true,
        stale: false,
        acknowledged,
        whole_files: pending.whole_files.into_iter().collect(),
    });
}

fn append_skim_summary(
    pending: &mut Option<PendingSkimFold>,
    next: PendingSkimFold,
    output: &mut Vec<SkimFoldSummary>,
    session: &ReviewSession,
    files: &[FileDiff],
) {
    if let Some(current) = pending.as_mut()
        && current.rationale == next.rationale
    {
        current.targets.extend(next.targets);
        current.paths.extend(next.paths);
        current.additions += next.additions;
        current.deletions += next.deletions;
        current.whole_files.extend(next.whole_files);
        return;
    }
    flush_skim_summary(pending, output, session, files);
    *pending = Some(next);
}

/// Derive current stream folds plus optional stale durable skim assignments
/// directly from the effective attention map. No cursor or ZenPhase state is
/// consulted, so every adapter observes the same identities and progress.
pub fn list_skim_folds(
    session: &ReviewSession,
    files: &[FileDiff],
    include_stale: bool,
) -> Vec<SkimFoldSummary> {
    let mut output = Vec::new();
    let mut pending = None;
    for file in files {
        let file_target = target_for_file_diff(file, &file.path, None, None).ok();
        let mut rows = Vec::new();
        for hunk in &file.hunks {
            for line in &hunk.lines {
                let Some(number) = line.new_lineno.or(line.old_lineno) else {
                    continue;
                };
                let Ok(target) = target_for_file_diff(file, &file.path, Some(number), None) else {
                    continue;
                };
                let effective = resolve_effective_attention(session, &target, files);
                rows.push((target, effective, line.kind));
            }
        }
        let whole_signature = rows.first().map(|(_, region, _)| fold_signature(region));
        let whole_file_skim = if rows.is_empty() {
            file_target.as_ref().is_some_and(|target| {
                resolve_effective_attention(session, target, files).salience == Salience::Skim
            })
        } else {
            whole_signature.as_ref().is_some_and(|signature| {
                signature.salience == Salience::Skim
                    && rows
                        .iter()
                        .all(|(_, region, _)| fold_signature(region) == *signature)
            })
        };
        if whole_file_skim {
            let effective = file_target
                .as_ref()
                .map(|target| resolve_effective_attention(session, target, files));
            let rationale = skim_rationale(
                rows.first()
                    .and_then(|(_, region, _)| region.rationale.clone())
                    .or_else(|| effective.and_then(|region| region.rationale)),
            );
            append_skim_summary(
                &mut pending,
                PendingSkimFold {
                    rationale,
                    targets: file_target.into_iter().collect(),
                    paths: BTreeSet::from([file.path.clone()]),
                    additions: file.additions,
                    deletions: file.deletions,
                    whole_files: BTreeSet::from([file.path.clone()]),
                },
                &mut output,
                session,
                files,
            );
            continue;
        }
        for (target, effective, kind) in rows {
            if effective.salience != Salience::Skim {
                flush_skim_summary(&mut pending, &mut output, session, files);
                continue;
            }
            append_skim_summary(
                &mut pending,
                PendingSkimFold {
                    rationale: skim_rationale(effective.rationale),
                    targets: vec![target],
                    paths: BTreeSet::from([file.path.clone()]),
                    additions: usize::from(matches!(kind, crate::diff::DiffLineKind::Added)),
                    deletions: usize::from(matches!(kind, crate::diff::DiffLineKind::Removed)),
                    whole_files: BTreeSet::new(),
                },
                &mut output,
                session,
                files,
            );
        }
    }
    flush_skim_summary(&mut pending, &mut output, session, files);

    if include_stale {
        let mut stale = BTreeMap::<String, SkimFoldSummary>::new();
        for region in session
            .attention_regions
            .iter()
            .filter(|region| region.salience == Salience::Skim && region_is_stale(region, files))
        {
            let target = AttentionProgressTarget::from_targets([&region.target]);
            let rationale = skim_rationale(region.rationale.clone());
            let paths = region.target.file.clone().into_iter().collect::<Vec<_>>();
            let id = stale_skim_fold_id(region);
            stale
                .entry(id.clone())
                .and_modify(|existing| {
                    if rationale < existing.rationale {
                        existing.rationale = rationale.clone();
                    }
                    existing.paths.extend(paths.clone());
                    existing.paths.sort();
                    existing.paths.dedup();
                    existing.file_count = existing.paths.len();
                })
                .or_insert(SkimFoldSummary {
                    id,
                    target,
                    file_count: paths.len(),
                    paths,
                    additions: 0,
                    deletions: 0,
                    rationale,
                    current: false,
                    stale: true,
                    acknowledged: false,
                    whole_files: Vec::new(),
                });
        }
        output.extend(stale.into_values());
    }
    output
}

fn selection_matches(selection: &SkimSelection, fold: &SkimFoldSummary) -> bool {
    match selection {
        SkimSelection::AllCurrent => fold.current && !fold.acknowledged,
        SkimSelection::StableId(id) => fold.id == *id || fold.id.starts_with(id),
        SkimSelection::Target {
            path,
            line,
            end_line,
        } => {
            let members = fold
                .target
                .members
                .iter()
                .filter(|member| member.file == *path)
                .collect::<Vec<_>>();
            let Some(selected_start) = *line else {
                return !members.is_empty();
            };
            let selected_end = end_line.unwrap_or(selected_start);
            let mut bounds = members.iter().filter_map(|member| {
                let start = member.line?;
                Some((start, member.end_line.unwrap_or(start)))
            });
            let Some((first_start, first_end)) = bounds.next() else {
                return false;
            };
            let (start, end) = bounds.fold(
                (first_start, first_end),
                |(minimum, maximum), (start, end)| (minimum.min(start), maximum.max(end)),
            );
            start == selected_start && end == selected_end
        }
    }
}

pub fn acknowledge_skim_folds(
    session: &mut ReviewSession,
    files: &[FileDiff],
    selection: &SkimSelection,
) -> Result<SkimAcknowledgeOutcome> {
    selection.validate()?;
    let folds = list_skim_folds(session, files, true);
    let matches = folds
        .iter()
        .filter(|fold| selection_matches(selection, fold))
        .cloned()
        .collect::<Vec<_>>();
    if matches.is_empty() && !matches!(selection, SkimSelection::AllCurrent) {
        return Err(match selection {
            SkimSelection::StableId(id) => eyre!(
                "unknown skim fold id `{id}`; run `gander attention skim-fold list` to list stable ids"
            ),
            SkimSelection::Target { path, line, .. } => match line {
                Some(line) => eyre!(
                    "no skim fold matches `{path}:{line}`; run `gander attention skim-fold list` to inspect current ranges"
                ),
                None => eyre!(
                    "no skim fold matches `{path}`; run `gander attention skim-fold list` to inspect current targets"
                ),
            },
            SkimSelection::AllCurrent => unreachable!("handled above"),
        });
    }
    if matches.len() > 1 && !matches!(selection, SkimSelection::AllCurrent) {
        return Err(match selection {
            SkimSelection::Target {
                path, line: None, ..
            } => eyre!(
                "skim target `{path}` is ambiguous across {} folds; provide --line/--end-line or --fold-id",
                matches.len()
            ),
            _ => eyre!(
                "skim selector is ambiguous across {} folds; provide an exact range or stable fold id",
                matches.len()
            ),
        });
    }
    let refs = files.iter().collect::<Vec<_>>();
    let mut outcome = SkimAcknowledgeOutcome {
        matched: matches.len(),
        ..Default::default()
    };
    let mut whole_files = BTreeSet::new();
    for fold in matches {
        if fold.stale || !fold.current {
            outcome.stale += 1;
            continue;
        }
        if fold.acknowledged {
            outcome.already_acknowledged += 1;
            continue;
        }
        record_attention_progress_refs(
            session,
            fold.target,
            AttentionProgressKind::SkimAcknowledged,
            &refs,
        )?;
        outcome.acknowledged += 1;
        whole_files.extend(fold.whole_files);
    }
    outcome.whole_files_viewed = whole_files.into_iter().collect();
    Ok(outcome)
}

/// Apply the conservative whole-file side effect returned by skim
/// acknowledgement. CLI and MCP both use this service; partial folds pass an
/// empty path list and therefore cannot mark a file viewed.
pub fn apply_whole_file_viewed_effects(
    state: &mut ReviewState,
    files: &[FileDiff],
    paths: &[String],
) {
    let paths = paths.iter().map(String::as_str).collect::<BTreeSet<_>>();
    for file in files
        .iter()
        .filter(|file| paths.contains(file.path.as_str()))
    {
        let saved = state.files.entry(file.path.clone()).or_default();
        saved.normalize_legacy();
        saved.fingerprint = file.fingerprint.clone();
        saved.viewed = true;
        saved.viewed_fingerprints.insert(file.fingerprint.clone());
        saved.caught_up_fingerprints.remove(&file.fingerprint);
    }
}

pub fn attention_coverage(session: &ReviewSession, files: &[FileDiff]) -> AttentionCoverage {
    let folds = list_skim_folds(session, files, false);
    let skim_acknowledged = folds.iter().filter(|fold| fold.acknowledged).count();
    let refs = files.iter().collect::<Vec<_>>();
    let mut spotlights = Vec::<AttentionProgressTarget>::new();
    for file in files {
        let mut pending = Vec::<ReviewTarget>::new();
        let mut signature = None;
        for hunk in &file.hunks {
            for line in &hunk.lines {
                let Some(number) = line.new_lineno.or(line.old_lineno) else {
                    continue;
                };
                let Ok(target) = target_for_file_diff(file, &file.path, Some(number), None) else {
                    continue;
                };
                let effective = resolve_effective_attention(session, &target, files);
                let next = fold_signature(&effective);
                if effective.salience == Salience::Spotlight {
                    if signature.as_ref().is_some_and(|current| current != &next)
                        && !pending.is_empty()
                    {
                        spotlights.push(AttentionProgressTarget::from_targets(pending.iter()));
                        pending.clear();
                    }
                    signature = Some(next);
                    pending.push(target);
                } else if !pending.is_empty() {
                    spotlights.push(AttentionProgressTarget::from_targets(pending.iter()));
                    pending.clear();
                    signature = None;
                }
            }
        }
        if !pending.is_empty() {
            spotlights.push(AttentionProgressTarget::from_targets(pending.iter()));
        }
    }
    spotlights.sort();
    spotlights.dedup();
    let spotlight_visited = spotlights
        .iter()
        .filter(|target| {
            attention_progress_is_current_refs(
                session,
                target,
                AttentionProgressKind::SpotlightVisited,
                &refs,
            )
        })
        .count();
    AttentionCoverage {
        covered: skim_acknowledged + spotlight_visited,
        total: folds.len() + spotlights.len(),
        skim_acknowledged,
        skim_total: folds.len(),
        spotlight_visited,
        spotlight_total: spotlights.len(),
    }
}

impl AttentionProgressTarget {
    /// Build a canonical progress identity from current region targets. Anchor
    /// evidence deliberately does not participate: it belongs in the separate
    /// current fingerprint, so a changed diff keeps history but stops counting.
    pub fn from_targets<'a>(targets: impl IntoIterator<Item = &'a ReviewTarget>) -> Self {
        let mut members = targets
            .into_iter()
            .filter_map(|target| {
                Some(AttentionProgressMember {
                    file: target.file.clone()?,
                    line: target.line,
                    end_line: target.line.map(|line| target.end_line.unwrap_or(line)),
                })
            })
            .collect::<Vec<_>>();
        members.sort();
        members.dedup();
        Self { members }
    }
}

/// Aggregate current diff fingerprints for a stable progress identity.
/// Missing files make the target stale and therefore return `None`.
pub fn progress_fingerprint(
    target: &AttentionProgressTarget,
    files: &[FileDiff],
) -> Option<String> {
    if target.members.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"gander-attention-progress-v1\0");
    for member in &target.members {
        let file = files.iter().find(|file| file.path == member.file)?;
        hasher.update(member.file.as_bytes());
        hasher.update(b"\0");
        hasher.update(member.line.unwrap_or(0).to_le_bytes());
        hasher.update(member.end_line.unwrap_or(0).to_le_bytes());
        hasher.update(file.fingerprint.as_bytes());
        hasher.update(b"\0");
    }
    Some(format!("{:x}", hasher.finalize()))
}

pub fn progress_fingerprint_refs(
    target: &AttentionProgressTarget,
    files: &[&FileDiff],
) -> Option<String> {
    if target.members.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"gander-attention-progress-v1\0");
    for member in &target.members {
        let file = files
            .iter()
            .copied()
            .find(|file| file.path == member.file)?;
        hasher.update(member.file.as_bytes());
        hasher.update(b"\0");
        hasher.update(member.line.unwrap_or(0).to_le_bytes());
        hasher.update(member.end_line.unwrap_or(0).to_le_bytes());
        hasher.update(file.fingerprint.as_bytes());
        hasher.update(b"\0");
    }
    Some(format!("{:x}", hasher.finalize()))
}

pub fn attention_progress_is_current_refs(
    session: &ReviewSession,
    target: &AttentionProgressTarget,
    kind: AttentionProgressKind,
    files: &[&FileDiff],
) -> bool {
    let Some(fingerprint) = progress_fingerprint_refs(target, files) else {
        return false;
    };
    session.attention_progress.iter().any(|progress| {
        progress.kind == kind && progress.target == *target && progress.fingerprint == fingerprint
    })
}

pub fn record_attention_progress_refs(
    session: &mut ReviewSession,
    target: AttentionProgressTarget,
    kind: AttentionProgressKind,
    files: &[&FileDiff],
) -> Result<bool> {
    let fingerprint = progress_fingerprint_refs(&target, files)
        .ok_or_else(|| eyre!("attention progress target is not current"))?;
    if session.attention_progress.iter().any(|progress| {
        progress.kind == kind && progress.target == target && progress.fingerprint == fingerprint
    }) {
        return Ok(false);
    }
    session.attention_progress.push(AttentionProgress {
        target,
        kind,
        fingerprint,
        recorded_at: chrono::Utc::now(),
    });
    session.updated_at = Some(chrono::Utc::now());
    Ok(true)
}

fn region_key(target: &ReviewTarget) -> Option<RegionKey> {
    Some(RegionKey {
        file: target.file.clone()?,
        line: target.line,
        end_line: target.line.map(|line| target.end_line.unwrap_or(line)),
    })
}

pub fn identity_target(
    path: &str,
    line: Option<usize>,
    end_line: Option<usize>,
) -> Result<ReviewTarget> {
    let target = ReviewTarget {
        file: Some(path.to_owned()),
        line,
        end_line,
        ..ReviewTarget::default()
    };
    validate_region_target(&target)?;
    Ok(target)
}

pub fn validate_target_anchor(target: &ReviewTarget) -> Result<()> {
    let Some(anchor) = &target.anchor else {
        return Ok(());
    };
    let file = target
        .file
        .as_deref()
        .filter(|file| !file.trim().is_empty())
        .ok_or_else(|| eyre!("anchored target must name a non-empty diff file"))?;
    if anchor.path() != file {
        return Err(eyre!("target path does not match its anchor"));
    }
    if anchor.line() != target.line {
        return Err(eyre!("target line does not match its anchor"));
    }
    let anchor_end = anchor.end_line().filter(|end| Some(*end) != anchor.line());
    let target_end = target.end_line.filter(|end| Some(*end) != target.line);
    if anchor_end != target_end {
        return Err(eyre!("target end_line does not match its anchor"));
    }
    Ok(())
}

pub fn validate_region_target(target: &ReviewTarget) -> Result<()> {
    let file = target
        .file
        .as_deref()
        .filter(|file| !file.trim().is_empty())
        .ok_or_else(|| eyre!("attention target must name a non-empty diff file"))?;
    if target.symbol.is_some() {
        return Err(eyre!(
            "attention target `{file}` must be a file or line range, not a symbol"
        ));
    }
    match (target.line, target.end_line) {
        (None, None) => {}
        (None, Some(_)) => return Err(eyre!("attention end_line requires line")),
        (Some(0), _) | (_, Some(0)) => {
            return Err(eyre!("attention line numbers are 1-indexed"));
        }
        (Some(start), Some(end)) if end < start => {
            return Err(eyre!(
                "attention end_line must be greater than or equal to line"
            ));
        }
        _ => {}
    }
    validate_target_anchor(target)?;
    Ok(())
}

pub fn validate_persisted_region(region: &AttentionRegion) -> Result<()> {
    validate_region_target(&region.target)?;
    if region.target.anchor.is_none() {
        return Err(eyre!(
            "durable attention target requires existing anchor/fingerprint evidence"
        ));
    }
    if region
        .rationale
        .as_deref()
        .is_some_and(|rationale| rationale.trim().is_empty())
    {
        return Err(eyre!("attention rationale must not be empty"));
    }
    Ok(())
}

pub fn target_for_diff(
    files: &[FileDiff],
    path: &str,
    line: Option<usize>,
    end_line: Option<usize>,
) -> Result<ReviewTarget> {
    let file = files
        .iter()
        .find(|file| file.path == path)
        .ok_or_else(|| eyre!("`{path}` is not a file in the current diff"))?;
    target_for_file_diff(file, path, line, end_line)
}

pub(crate) fn target_for_file_diff(
    file: &FileDiff,
    path: &str,
    line: Option<usize>,
    end_line: Option<usize>,
) -> Result<ReviewTarget> {
    let mut target = ReviewTarget {
        file: Some(path.to_owned()),
        line,
        end_line,
        ..ReviewTarget::default()
    };
    validate_region_target(&target)?;
    let anchor = comment_anchor_for_file_diff(file, line, end_line)
        .ok_or_else(|| eyre!("attention target {path} is outside current diff line space"))?;
    target.anchor = Some(anchor);
    validate_persisted_region(&AttentionRegion {
        target: target.clone(),
        salience: Salience::Supporting,
        rationale: None,
        source: SalienceSource::Human,
    })?;
    Ok(target)
}

/// Whether a durable target still names the same fingerprint-valid current
/// diff region. Anchorless legacy walkthrough steps are intentionally not
/// guessed current.
pub fn target_is_current(target: &ReviewTarget, files: &[&FileDiff]) -> bool {
    let Some(path) = target.file.as_deref() else {
        return false;
    };
    let Some(anchor) = target.anchor.as_ref() else {
        return false;
    };
    let Some(file) = files.iter().copied().find(|file| file.path == path) else {
        return false;
    };
    let Ok(current) = target_for_file_diff(file, path, target.line, target.end_line) else {
        return false;
    };
    current.anchor.as_ref().is_some_and(|current_anchor| {
        anchor.path() == current_anchor.path()
            && anchor_diff_fingerprint(anchor) == anchor_diff_fingerprint(current_anchor)
    })
}

fn anchor_diff_fingerprint(anchor: &CommentAnchor) -> &str {
    match anchor {
        CommentAnchor::File {
            diff_fingerprint, ..
        }
        | CommentAnchor::Line {
            diff_fingerprint, ..
        }
        | CommentAnchor::Range {
            diff_fingerprint, ..
        } => diff_fingerprint,
    }
}

pub fn region_is_stale(region: &AttentionRegion, files: &[FileDiff]) -> bool {
    let refs = files.iter().collect::<Vec<_>>();
    region_is_stale_refs(region, &refs)
}

fn region_is_stale_refs(region: &AttentionRegion, files: &[&FileDiff]) -> bool {
    if validate_persisted_region(region).is_err() {
        return true;
    }
    let Some(path) = region.target.file.as_deref() else {
        return true;
    };
    let Some(anchor) = region.target.anchor.as_ref() else {
        return true;
    };
    let Some(file) = files.iter().copied().find(|file| file.path == path) else {
        return true;
    };
    anchor.path() != path
        || anchor_diff_fingerprint(anchor) != file.fingerprint
        || target_for_file_diff(file, path, region.target.line, region.target.end_line).is_err()
}

fn source_rank(source: SalienceSource) -> u8 {
    match source {
        SalienceSource::Human => 3,
        SalienceSource::Agent => 2,
        SalienceSource::Heuristic => 1,
    }
}

fn salience_rank(salience: Salience) -> u8 {
    match salience {
        Salience::Spotlight => 3,
        Salience::Supporting => 2,
        Salience::Skim => 1,
    }
}

fn range_width(target: &ReviewTarget) -> usize {
    target
        .line
        .map(|start| target.end_line.unwrap_or(start).saturating_sub(start) + 1)
        .unwrap_or(usize::MAX)
}

fn covers(candidate: &ReviewTarget, query: &ReviewTarget) -> bool {
    if candidate.file != query.file {
        return false;
    }
    match (candidate.line, query.line) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(candidate_start), Some(query_start)) => {
            let candidate_end = candidate.end_line.unwrap_or(candidate_start);
            let query_end = query.end_line.unwrap_or(query_start);
            candidate_start <= query_start && candidate_end >= query_end
        }
    }
}

fn compare_candidates(left: &AttentionRegion, right: &AttentionRegion) -> Ordering {
    source_rank(left.source)
        .cmp(&source_rank(right.source))
        .then_with(|| left.target.line.is_some().cmp(&right.target.line.is_some()))
        .then_with(|| range_width(&right.target).cmp(&range_width(&left.target)))
        .then_with(|| salience_rank(left.salience).cmp(&salience_rank(right.salience)))
        .then_with(|| region_key(&right.target).cmp(&region_key(&left.target)))
        .then_with(|| right.rationale.cmp(&left.rationale))
}

/// Resolve effective attention for one file/range query.
///
/// Stale assignments are excluded. Human source precedence is evaluated before
/// specificity, so a human file override intentionally outranks an agent hunk.
pub fn resolve_effective_attention(
    session: &ReviewSession,
    query: &ReviewTarget,
    files: &[FileDiff],
) -> EffectiveAttentionRegion {
    let refs = files.iter().collect::<Vec<_>>();
    resolve_effective_attention_refs(session, query, &refs)
}

/// Borrowed-file resolver for render paths; avoids cloning full diffs merely
/// to evaluate current attention.
pub fn resolve_effective_attention_refs(
    session: &ReviewSession,
    query: &ReviewTarget,
    files: &[&FileDiff],
) -> EffectiveAttentionRegion {
    let winner = session
        .attention_regions
        .iter()
        .filter(|region| !region_is_stale_refs(region, files))
        .filter(|region| covers(&region.target, query))
        .max_by(|left, right| compare_candidates(left, right));
    EffectiveAttentionRegion {
        target: query.clone(),
        salience: winner.map_or(Salience::Supporting, |region| region.salience),
        rationale: winner.and_then(|region| region.rationale.clone()),
        source: winner.map(|region| region.source),
    }
}

pub fn list_assigned_attention(
    session: &ReviewSession,
    files: &[FileDiff],
) -> Vec<AssignedAttentionRegion> {
    let mut regions = session
        .attention_regions
        .iter()
        .map(|region| assigned_attention_region(region, files))
        .collect::<Vec<_>>();
    regions.sort_by(|left, right| {
        region_key(&left.target)
            .cmp(&region_key(&right.target))
            .then_with(|| source_rank(right.source).cmp(&source_rank(left.source)))
            .then_with(|| salience_rank(right.salience).cmp(&salience_rank(left.salience)))
            .then_with(|| left.rationale.cmp(&right.rationale))
    });
    regions
}

pub fn list_effective_attention(
    session: &ReviewSession,
    files: &[FileDiff],
) -> Vec<EffectiveAttentionRegion> {
    let mut effective = Vec::new();
    for file in files {
        let file_target = target_for_diff(files, &file.path, None, None)
            .expect("current diff file must produce a file target");
        effective.push(resolve_effective_attention(session, &file_target, files));

        let mut current_ranges = session
            .attention_regions
            .iter()
            .filter(|region| !region_is_stale(region, files))
            .filter(|region| region.target.file.as_deref() == Some(file.path.as_str()))
            .filter_map(|region| {
                let start = region.target.line?;
                Some((
                    region.source,
                    start,
                    region.target.end_line.unwrap_or(start),
                ))
            })
            .collect::<Vec<_>>();
        current_ranges.sort_by_key(|(source, start, end)| (source_rank(*source), *start, *end));
        let rows = anchorable_line_numbers(file);
        let mut pending: Option<PendingEffectiveSpan> = None;
        for line in rows {
            let active = current_ranges
                .iter()
                .filter(|(_, start, end)| *start <= line && *end >= line)
                .map(|(source, start, end)| (*source, *start, *end))
                .collect::<Vec<_>>();
            if active.is_empty() {
                flush_effective_span(&mut effective, pending.take(), files, &file.path);
                continue;
            }
            let query = target_for_diff(files, &file.path, Some(line), None)
                .expect("anchorable row must produce a line target");
            let resolved = resolve_effective_attention(session, &query, files);
            let signature = EffectiveSpanSignature {
                salience: resolved.salience,
                rationale: resolved.rationale,
                source: resolved.source,
                active,
            };
            let consecutive = pending.as_ref().is_some_and(|span| {
                span.end.checked_add(1) == Some(line) && span.signature == signature
            });
            if consecutive {
                pending.as_mut().expect("checked above").end = line;
            } else {
                flush_effective_span(&mut effective, pending.take(), files, &file.path);
                pending = Some(PendingEffectiveSpan {
                    start: line,
                    end: line,
                    signature,
                });
            }
        }
        flush_effective_span(&mut effective, pending, files, &file.path);
    }
    effective
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectiveSpanSignature {
    salience: Salience,
    rationale: Option<String>,
    source: Option<SalienceSource>,
    active: Vec<(SalienceSource, usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingEffectiveSpan {
    start: usize,
    end: usize,
    signature: EffectiveSpanSignature,
}

fn anchorable_line_numbers(file: &FileDiff) -> Vec<usize> {
    let mut lines = file
        .hunks
        .iter()
        .flat_map(|hunk| hunk.lines.iter())
        .filter_map(|line| line.new_lineno.or(line.old_lineno))
        .collect::<Vec<_>>();
    lines.sort_unstable();
    lines.dedup();
    lines
}

fn flush_effective_span(
    output: &mut Vec<EffectiveAttentionRegion>,
    span: Option<PendingEffectiveSpan>,
    files: &[FileDiff],
    path: &str,
) {
    let Some(span) = span else { return };
    let end_line = (span.end != span.start).then_some(span.end);
    let target = target_for_diff(files, path, Some(span.start), end_line)
        .expect("grouped anchorable rows must produce a range target");
    output.push(EffectiveAttentionRegion {
        target,
        salience: span.signature.salience,
        rationale: span.signature.rationale,
        source: span.signature.source,
    });
}

pub fn assigned_attention_region(
    region: &AttentionRegion,
    files: &[FileDiff],
) -> AssignedAttentionRegion {
    AssignedAttentionRegion {
        target: region.target.clone(),
        salience: region.salience,
        rationale: region.rationale.clone(),
        source: region.source,
        stale: region_is_stale(region, files),
    }
}

fn normalize_rationale(rationale: Option<String>) -> Result<Option<String>> {
    match rationale {
        Some(value) if value.trim().is_empty() => {
            Err(eyre!("attention rationale must not be empty"))
        }
        Some(value) => Ok(Some(value.trim().to_owned())),
        None => Ok(None),
    }
}

pub fn set_human_attention(
    session: &mut ReviewSession,
    target: ReviewTarget,
    salience: Salience,
    rationale: Option<String>,
) -> Result<AttentionRegion> {
    validate_persisted_region(&AttentionRegion {
        target: target.clone(),
        salience,
        rationale: rationale.clone(),
        source: SalienceSource::Human,
    })?;
    let rationale = normalize_rationale(rationale)?;
    let key = region_key(&target).expect("validated target has key");
    session.attention_regions.retain(|region| {
        region.source != SalienceSource::Human || region_key(&region.target) != Some(key.clone())
    });
    let region = AttentionRegion {
        target,
        salience,
        rationale,
        source: SalienceSource::Human,
    };
    session.attention_regions.push(region.clone());
    session.updated_at = Some(chrono::Utc::now());
    Ok(region)
}

pub fn clear_human_attention(session: &mut ReviewSession, target: &ReviewTarget) -> bool {
    let key = region_key(target);
    let before = session.attention_regions.len();
    session.attention_regions.retain(|region| {
        region.source != SalienceSource::Human || region_key(&region.target) != key
    });
    let cleared = before != session.attention_regions.len();
    if cleared {
        session.updated_at = Some(chrono::Utc::now());
    }
    cleared
}

pub fn promote_human_attention(
    session: &mut ReviewSession,
    target: ReviewTarget,
    rationale: Option<String>,
    files: &[FileDiff],
) -> Result<AttentionRegion> {
    let salience = resolve_effective_attention(session, &target, files)
        .salience
        .promote();
    let rationale = rationale.or_else(|| existing_human_rationale(session, &target));
    set_human_attention(session, target, salience, rationale)
}

pub fn promote_human_attention_refs(
    session: &mut ReviewSession,
    target: ReviewTarget,
    rationale: Option<String>,
    files: &[&FileDiff],
) -> Result<AttentionRegion> {
    let salience = resolve_effective_attention_refs(session, &target, files)
        .salience
        .promote();
    let rationale = rationale.or_else(|| existing_human_rationale(session, &target));
    set_human_attention(session, target, salience, rationale)
}

pub fn demote_human_attention(
    session: &mut ReviewSession,
    target: ReviewTarget,
    rationale: Option<String>,
    files: &[FileDiff],
) -> Result<AttentionRegion> {
    let salience = resolve_effective_attention(session, &target, files)
        .salience
        .demote();
    let rationale = rationale.or_else(|| existing_human_rationale(session, &target));
    set_human_attention(session, target, salience, rationale)
}

pub fn demote_human_attention_refs(
    session: &mut ReviewSession,
    target: ReviewTarget,
    rationale: Option<String>,
    files: &[&FileDiff],
) -> Result<AttentionRegion> {
    let salience = resolve_effective_attention_refs(session, &target, files)
        .salience
        .demote();
    let rationale = rationale.or_else(|| existing_human_rationale(session, &target));
    set_human_attention(session, target, salience, rationale)
}

fn existing_human_rationale(session: &ReviewSession, target: &ReviewTarget) -> Option<String> {
    let key = region_key(target)?;
    session
        .attention_regions
        .iter()
        .find(|region| {
            region.source == SalienceSource::Human
                && region_key(&region.target) == Some(key.clone())
        })
        .and_then(|region| region.rationale.clone())
}

pub fn sync_agent_attention(session: &mut ReviewSession, files: &[FileDiff]) -> Result<usize> {
    let before = session.attention_regions.clone();
    let file_refs = files.iter().collect::<Vec<_>>();
    let mut source_keys = BTreeSet::new();
    let mut desired = Vec::<AttentionRegion>::new();
    for step in session
        .walkthroughs
        .iter()
        .flat_map(|walkthrough| walkthrough.steps.iter())
        .filter(|step| step.kind != StepKind::Chapter)
    {
        for step_target in std::iter::once(&step.target).chain(step.extra_targets.iter()) {
            let Some(key) = region_key(step_target) else {
                continue;
            };
            source_keys.insert(key);
            let path = step_target
                .file
                .as_deref()
                .expect("region key requires file");
            let Ok(mut target) =
                target_for_diff(files, path, step_target.line, step_target.end_line)
            else {
                // A source still exists, so an older durable Agent assignment
                // remains unchanged/stale until the source is removed or can
                // be re-anchored later.
                continue;
            };
            if let Some(existing_anchor) = step_target.anchor.clone() {
                let mut anchored = target.clone();
                anchored.anchor = Some(existing_anchor);
                if validate_target_anchor(&anchored).is_ok() {
                    target = anchored;
                }
            }
            let region = AttentionRegion {
                target,
                salience: match step.importance {
                    StepImportance::Spotlight => Salience::Spotlight,
                    StepImportance::Glance => Salience::Skim,
                },
                rationale: first_non_empty([&step.why, &step.body, &step.title]),
                source: SalienceSource::Agent,
            };
            validate_persisted_region(&region)?;
            let key = region_key(&region.target).expect("derived target has key");
            if let Some(existing) = desired
                .iter_mut()
                .find(|candidate| region_key(&candidate.target) == Some(key.clone()))
            {
                if compare_candidates(existing, &region) == Ordering::Less {
                    *existing = region;
                }
            } else {
                desired.push(region);
            }
        }
    }

    let mut next = session.attention_regions.clone();
    next.retain(|region| {
        region.source != SalienceSource::Agent
            || region_key(&region.target).is_some_and(|key| source_keys.contains(&key))
    });
    for mut region in desired {
        let key = region_key(&region.target).expect("derived target has key");
        if let Some(existing) = next.iter_mut().find(|candidate| {
            candidate.source == SalienceSource::Agent
                && region_key(&candidate.target) == Some(key.clone())
        }) {
            // Fingerprint drift stays stale until a future re-anchor operation;
            // changing walkthrough prose/importance must not acknowledge drift.
            if existing.target.anchor.is_some() && !target_is_current(&region.target, &file_refs) {
                region.target = existing.target.clone();
            }
            if validate_persisted_region(&region).is_ok() {
                *existing = region;
            }
        } else {
            next.push(region);
        }
    }
    if next != before {
        session.attention_regions = next;
        session.updated_at = Some(chrono::Utc::now());
    }
    Ok(source_keys.len())
}

fn first_non_empty<const N: usize>(values: [&Option<String>; N]) -> Option<String> {
    values
        .into_iter()
        .filter_map(|value| value.as_deref())
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

fn ignore_matcher(patterns: &[String]) -> Result<globset::GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
    }
    Ok(builder.build()?)
}

fn heuristic_rationale(
    file: &FileDiff,
    generated: &GeneratedMatcher,
    lockfiles: &GeneratedMatcher,
    ignored: &globset::GlobSet,
) -> Option<String> {
    let mut reasons = Vec::new();
    if lockfiles.is_match(&file.path) {
        reasons.push("lockfile");
    }
    if generated.is_match(&file.path) {
        reasons.push("generated path policy");
    }
    if diff_content_looks_generated(file) {
        reasons.push("generated content marker");
    }
    if ignored.is_match(&file.path) {
        reasons.push("ignore policy");
    }
    (!reasons.is_empty()).then(|| format!("Skim: {}", reasons.join("; ")))
}

pub fn update_heuristic_attention(
    session: &mut ReviewSession,
    files: &[FileDiff],
    generated_policy: &GeneratedPolicy,
    ignore_globs: &[String],
    recompute: bool,
) -> Result<HeuristicUpdate> {
    let generated = GeneratedMatcher::new(generated_policy)?;
    let lockfiles = GeneratedMatcher::new(&GeneratedPolicy {
        presets: vec![GeneratedPreset::Lockfiles],
        globs: Vec::new(),
    })?;
    let ignored = ignore_matcher(ignore_globs)?;
    let candidates = files
        .iter()
        .filter_map(|file| {
            heuristic_rationale(file, &generated, &lockfiles, &ignored)
                .map(|rationale| (file, rationale))
        })
        .collect::<Vec<_>>();
    let candidate_paths = candidates
        .iter()
        .map(|(file, _)| file.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut update = HeuristicUpdate {
        added: 0,
        updated: 0,
        removed: 0,
        preserved_stale: 0,
    };

    if recompute {
        session.attention_regions.retain(|region| {
            if region.source != SalienceSource::Heuristic {
                return true;
            }
            if region_is_stale(region, files) {
                update.preserved_stale += 1;
                return true;
            }
            let keep = region
                .target
                .file
                .as_deref()
                .is_some_and(|path| candidate_paths.contains(path));
            if !keep {
                update.removed += 1;
            }
            keep
        });
    }

    for (file, rationale) in candidates {
        let target = target_for_diff(files, &file.path, None, None)?;
        let key = region_key(&target).expect("file target has key");
        if let Some(existing) = session.attention_regions.iter_mut().find(|region| {
            region.source == SalienceSource::Heuristic
                && region_key(&region.target) == Some(key.clone())
        }) {
            if region_is_stale(existing, files) {
                if !recompute {
                    update.preserved_stale += 1;
                }
                continue;
            }
            if existing.salience != Salience::Skim
                || existing.rationale.as_deref() != Some(rationale.as_str())
            {
                existing.salience = Salience::Skim;
                existing.rationale = Some(rationale);
                update.updated += 1;
            }
        } else {
            session.attention_regions.push(AttentionRegion {
                target,
                salience: Salience::Skim,
                rationale: Some(rationale),
                source: SalienceSource::Heuristic,
            });
            update.added += 1;
        }
    }
    if update.added + update.updated + update.removed > 0 {
        session.updated_at = Some(chrono::Utc::now());
    }
    Ok(update)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffLine, DiffLineKind, DiffSet, FileStatus, Hunk};

    fn files() -> Vec<FileDiff> {
        DiffSet::parse(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n old\n-new\n+newer\n tail\n",
        )
        .unwrap()
        .files
    }

    fn ten_line_files() -> Vec<FileDiff> {
        DiffSet::parse(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,10 +1,10 @@\n line1\n line2\n line3\n line4\n line5\n line6\n line7\n line8\n line9\n line10\n",
        )
        .unwrap()
        .files
    }

    fn sparse_files() -> Vec<FileDiff> {
        DiffSet::parse(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n one\n two\n@@ -100,2 +100,2 @@\n hundred\n hundred-one\n",
        )
        .unwrap()
        .files
    }

    fn region(
        files: &[FileDiff],
        source: SalienceSource,
        salience: Salience,
        line: Option<usize>,
        end_line: Option<usize>,
    ) -> AttentionRegion {
        AttentionRegion {
            target: target_for_diff(files, "src/lib.rs", line, end_line).unwrap(),
            salience,
            rationale: None,
            source,
        }
    }

    #[test]
    fn progress_is_append_only_but_fingerprint_drift_stops_counting() {
        let files = files();
        let target = target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap();
        let file_refs = files.iter().collect::<Vec<_>>();
        let progress_target = AttentionProgressTarget::from_targets([&target]);
        let mut session = ReviewSession::default();
        assert!(
            record_attention_progress_refs(
                &mut session,
                progress_target.clone(),
                AttentionProgressKind::SpotlightVisited,
                &file_refs,
            )
            .unwrap()
        );
        assert!(attention_progress_is_current_refs(
            &session,
            &progress_target,
            AttentionProgressKind::SpotlightVisited,
            &file_refs,
        ));
        let drifted = DiffSet::parse(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n old\n-new\n+different\n tail\n",
        )
        .unwrap()
        .files;
        let drifted_refs = drifted.iter().collect::<Vec<_>>();
        assert!(!attention_progress_is_current_refs(
            &session,
            &progress_target,
            AttentionProgressKind::SpotlightVisited,
            &drifted_refs,
        ));
        assert_eq!(session.attention_progress.len(), 1, "history is retained");
    }

    #[test]
    fn skim_fold_service_lists_acknowledges_and_applies_whole_file_semantics() {
        let files = files();
        let mut session = ReviewSession {
            attention_regions: vec![region(
                &files,
                SalienceSource::Human,
                Salience::Skim,
                None,
                None,
            )],
            ..Default::default()
        };
        let folds = list_skim_folds(&session, &files, true);
        assert_eq!(folds.len(), 1);
        assert!(folds[0].current);
        assert!(!folds[0].stale);
        assert_eq!(folds[0].whole_files, ["src/lib.rs"]);

        let outcome = acknowledge_skim_folds(
            &mut session,
            &files,
            &SkimSelection::StableId(folds[0].id.clone()),
        )
        .unwrap();
        assert_eq!(outcome.acknowledged, 1);
        assert_eq!(outcome.whole_files_viewed, ["src/lib.rs"]);
        assert!(list_skim_folds(&session, &files, false)[0].acknowledged);
        let coverage = attention_coverage(&session, &files);
        assert_eq!((coverage.covered, coverage.total), (1, 1));
    }

    #[test]
    fn partial_and_stale_skims_never_claim_whole_file_progress() {
        let files = files();
        let mut session = ReviewSession {
            attention_regions: vec![region(
                &files,
                SalienceSource::Human,
                Salience::Skim,
                Some(2),
                None,
            )],
            ..Default::default()
        };
        let fold = list_skim_folds(&session, &files, false)
            .into_iter()
            .next()
            .unwrap();
        assert!(fold.whole_files.is_empty());
        let outcome = acknowledge_skim_folds(
            &mut session,
            &files,
            &SkimSelection::Target {
                path: "src/lib.rs".into(),
                line: Some(2),
                end_line: None,
            },
        )
        .unwrap();
        assert_eq!(outcome.acknowledged, 1);
        assert!(outcome.whole_files_viewed.is_empty());

        session.attention_regions[0].target.anchor = None;
        let stale = list_skim_folds(&session, &files, true)
            .into_iter()
            .find(|fold| fold.stale)
            .unwrap();
        let outcome =
            acknowledge_skim_folds(&mut session, &files, &SkimSelection::StableId(stale.id))
                .unwrap();
        assert_eq!(outcome.stale, 1);
        assert_eq!(outcome.acknowledged, 0);
    }

    #[test]
    fn explicit_skim_selectors_reject_unknown_and_ambiguous_targets() {
        let files = ten_line_files();
        let mut session = ReviewSession {
            attention_regions: vec![
                region(
                    &files,
                    SalienceSource::Human,
                    Salience::Skim,
                    Some(2),
                    Some(3),
                ),
                region(
                    &files,
                    SalienceSource::Human,
                    Salience::Skim,
                    Some(7),
                    Some(8),
                ),
            ],
            ..Default::default()
        };
        let unknown_id = acknowledge_skim_folds(
            &mut session,
            &files,
            &SkimSelection::StableId("fold:missing".into()),
        )
        .unwrap_err()
        .to_string();
        assert!(unknown_id.contains("unknown skim fold id"), "{unknown_id}");
        let unknown_path = acknowledge_skim_folds(
            &mut session,
            &files,
            &SkimSelection::Target {
                path: "missing.rs".into(),
                line: None,
                end_line: None,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            unknown_path.contains("no skim fold matches"),
            "{unknown_path}"
        );
        let ambiguous = acknowledge_skim_folds(
            &mut session,
            &files,
            &SkimSelection::Target {
                path: "src/lib.rs".into(),
                line: None,
                end_line: None,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            ambiguous.contains("ambiguous across 2 folds"),
            "{ambiguous}"
        );
        assert!(ambiguous.contains("--line/--end-line or --fold-id"));

        let selected = acknowledge_skim_folds(
            &mut session,
            &files,
            &SkimSelection::Target {
                path: "src/lib.rs".into(),
                line: Some(2),
                end_line: Some(3),
            },
        )
        .unwrap();
        assert_eq!(selected.acknowledged, 1);

        let empty = acknowledge_skim_folds(
            &mut ReviewSession::default(),
            &files,
            &SkimSelection::AllCurrent,
        )
        .unwrap();
        assert_eq!(empty.matched, 0, "explicit bulk on an empty set is a no-op");
    }

    #[test]
    fn stale_fold_ids_include_source_and_anchor_and_aggregate_identical_rows() {
        let files = files();
        let mut human = region(&files, SalienceSource::Human, Salience::Skim, Some(2), None);
        let mut agent = human.clone();
        agent.source = SalienceSource::Agent;
        human.target.anchor = None;
        agent.target.anchor = None;
        let duplicate_human = human.clone();
        let mut session = ReviewSession {
            attention_regions: vec![human.clone(), agent.clone(), duplicate_human],
            ..Default::default()
        };
        let first = list_skim_folds(&session, &files, true)
            .into_iter()
            .filter(|fold| fold.stale)
            .map(|fold| fold.id)
            .collect::<Vec<_>>();
        assert_eq!(first.len(), 2, "identical malformed rows aggregate");
        assert_ne!(first[0], first[1], "source participates in stale identity");
        session.attention_regions.reverse();
        let reversed = list_skim_folds(&session, &files, true)
            .into_iter()
            .filter(|fold| fold.stale)
            .map(|fold| fold.id)
            .collect::<Vec<_>>();
        assert_eq!(
            first, reversed,
            "stale board ordering and ids are deterministic"
        );
        let prefix = &first[0][..first[0].len() - 8];
        let outcome = acknowledge_skim_folds(
            &mut session,
            &files,
            &SkimSelection::StableId(prefix.into()),
        )
        .unwrap();
        assert_eq!(outcome.stale, 1, "unique stale prefixes resolve stably");

        let mut anchored_a = region(&files, SalienceSource::Human, Salience::Skim, Some(2), None);
        let mut anchored_b = anchored_a.clone();
        for (region, fingerprint) in [(&mut anchored_a, "old-a"), (&mut anchored_b, "old-b")] {
            match region.target.anchor.as_mut().unwrap() {
                CommentAnchor::File {
                    diff_fingerprint, ..
                }
                | CommentAnchor::Line {
                    diff_fingerprint, ..
                }
                | CommentAnchor::Range {
                    diff_fingerprint, ..
                } => *diff_fingerprint = fingerprint.into(),
            }
        }
        let anchored = ReviewSession {
            attention_regions: vec![anchored_a, anchored_b],
            ..Default::default()
        };
        let ids = list_skim_folds(&anchored, &files, true)
            .into_iter()
            .filter(|fold| fold.stale)
            .map(|fold| fold.id)
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 2);
        assert_ne!(
            ids[0], ids[1],
            "anchor fingerprint participates in identity"
        );
    }

    #[test]
    fn source_precedence_outranks_specificity() {
        let files = files();
        let session = ReviewSession {
            attention_regions: vec![
                region(
                    &files,
                    SalienceSource::Heuristic,
                    Salience::Skim,
                    Some(2),
                    None,
                ),
                region(
                    &files,
                    SalienceSource::Agent,
                    Salience::Spotlight,
                    Some(2),
                    None,
                ),
                region(&files, SalienceSource::Human, Salience::Skim, None, None),
            ],
            ..Default::default()
        };
        let query = target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap();
        let effective = resolve_effective_attention(&session, &query, &files);
        assert_eq!(effective.salience, Salience::Skim);
        assert_eq!(effective.source, Some(SalienceSource::Human));

        let mut without_human = session.clone();
        without_human
            .attention_regions
            .retain(|region| region.source != SalienceSource::Human);
        let effective = resolve_effective_attention(&without_human, &query, &files);
        assert_eq!(effective.salience, Salience::Spotlight);
        assert_eq!(effective.source, Some(SalienceSource::Agent));
    }

    #[test]
    fn target_validation_requires_current_file_or_exact_ordered_diff_range() {
        let files = files();
        assert!(target_for_diff(&files, "missing.rs", None, None).is_err());
        assert!(target_for_diff(&files, "src/lib.rs", Some(3), Some(2)).is_err());
        assert!(target_for_diff(&files, "src/lib.rs", Some(99), None).is_err());

        let range = target_for_diff(&files, "src/lib.rs", Some(2), Some(3)).unwrap();
        assert_eq!(range.line, Some(2));
        assert_eq!(range.end_line, Some(3));
        assert!(matches!(range.anchor, Some(CommentAnchor::Range { .. })));
    }

    #[test]
    fn narrower_region_wins_within_same_source() {
        let files = files();
        let session = ReviewSession {
            attention_regions: vec![
                region(&files, SalienceSource::Agent, Salience::Skim, None, None),
                region(
                    &files,
                    SalienceSource::Agent,
                    Salience::Spotlight,
                    Some(2),
                    Some(3),
                ),
                region(&files, SalienceSource::Agent, Salience::Skim, Some(2), None),
            ],
            ..Default::default()
        };
        let query = target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap();
        assert_eq!(
            resolve_effective_attention(&session, &query, &files).salience,
            Salience::Skim
        );
    }

    #[test]
    fn malformed_duplicate_tie_is_order_independent_and_prefers_higher_salience() {
        let files = files();
        let skim = region(&files, SalienceSource::Agent, Salience::Skim, Some(2), None);
        let spotlight = region(
            &files,
            SalienceSource::Agent,
            Salience::Spotlight,
            Some(2),
            None,
        );
        let query = target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap();
        for attention_regions in [
            vec![skim.clone(), spotlight.clone()],
            vec![spotlight.clone(), skim.clone()],
        ] {
            let session = ReviewSession {
                attention_regions,
                ..Default::default()
            };
            assert_eq!(
                resolve_effective_attention(&session, &query, &files).salience,
                Salience::Spotlight
            );
        }
    }

    #[test]
    fn stale_region_is_retained_but_not_effective() {
        let original = files();
        let session = ReviewSession {
            attention_regions: vec![region(
                &original,
                SalienceSource::Human,
                Salience::Skim,
                None,
                None,
            )],
            ..Default::default()
        };
        let changed = DiffSet::parse(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+different\n",
        )
        .unwrap()
        .files;
        assert!(region_is_stale(&session.attention_regions[0], &changed));
        let query = target_for_diff(&changed, "src/lib.rs", None, None).unwrap();
        assert_eq!(
            resolve_effective_attention(&session, &query, &changed).salience,
            Salience::Supporting
        );
        assert_eq!(session.attention_regions.len(), 1);
    }

    #[test]
    fn malformed_matching_fingerprint_regions_are_stale_and_ineffective() {
        let files = files();
        let malformed = AttentionRegion {
            target: ReviewTarget {
                file: Some("src/lib.rs".into()),
                line: Some(2),
                anchor: Some(CommentAnchor::File {
                    path: "src/lib.rs".into(),
                    old_path: None,
                    diff_fingerprint: files[0].fingerprint.clone(),
                }),
                ..Default::default()
            },
            salience: Salience::Skim,
            rationale: None,
            source: SalienceSource::Human,
        };
        let malformed_coordinates = AttentionRegion {
            target: ReviewTarget {
                file: Some("src/lib.rs".into()),
                end_line: Some(2),
                anchor: Some(CommentAnchor::File {
                    path: "src/lib.rs".into(),
                    old_path: None,
                    diff_fingerprint: files[0].fingerprint.clone(),
                }),
                ..Default::default()
            },
            ..malformed.clone()
        };
        let session = ReviewSession {
            attention_regions: vec![malformed.clone(), malformed_coordinates.clone()],
            ..Default::default()
        };
        assert!(region_is_stale(&malformed, &files));
        assert!(region_is_stale(&malformed_coordinates, &files));
        let query = target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap();
        assert_eq!(
            resolve_effective_attention(&session, &query, &files).salience,
            Salience::Supporting
        );

        let older = chrono::Utc::now();
        let mut local = crate::state::ReviewState {
            sessions: vec![ReviewSession {
                id: "session".into(),
                updated_at: Some(older),
                ..Default::default()
            }],
            ..Default::default()
        };
        let external = crate::state::ReviewState {
            sessions: vec![ReviewSession {
                id: "session".into(),
                attention_regions: vec![malformed],
                updated_at: Some(older + chrono::TimeDelta::seconds(1)),
                ..Default::default()
            }],
            ..Default::default()
        };
        local.merge_external(external, &crate::state::ReviewStateTombstones::default());
        assert!(region_is_stale(
            &local.sessions[0].attention_regions[0],
            &files
        ));
    }

    #[test]
    fn unrelated_edit_in_same_file_makes_line_region_stale() {
        let original = files();
        let region = region(
            &original,
            SalienceSource::Human,
            Salience::Spotlight,
            Some(2),
            None,
        );
        let changed = DiffSet::parse(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n old\n-new\n+newer\n-tail\n+different tail\n",
        )
        .unwrap()
        .files;
        assert!(region_is_stale(&region, &changed));
    }

    #[test]
    fn effective_list_partitions_overlaps_at_every_assignment_boundary() {
        let files = ten_line_files();
        let session = ReviewSession {
            attention_regions: vec![
                region(
                    &files,
                    SalienceSource::Agent,
                    Salience::Skim,
                    Some(1),
                    Some(10),
                ),
                region(
                    &files,
                    SalienceSource::Agent,
                    Salience::Spotlight,
                    Some(5),
                    None,
                ),
            ],
            ..Default::default()
        };
        let effective = list_effective_attention(&session, &files);
        let broad = target_for_diff(&files, "src/lib.rs", Some(1), Some(10)).unwrap();
        assert_eq!(
            resolve_effective_attention(&session, &broad, &files).salience,
            Salience::Skim
        );
        let spans = effective
            .iter()
            .filter(|region| region.target.line.is_some())
            .map(|region| {
                (
                    region.target.line.unwrap(),
                    region
                        .target
                        .end_line
                        .unwrap_or(region.target.line.unwrap()),
                    region.salience,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            spans,
            [
                (1, 4, Salience::Skim),
                (5, 5, Salience::Spotlight),
                (6, 10, Salience::Skim),
            ]
        );
    }

    #[test]
    fn effective_partition_applies_source_precedence_per_disjoint_span() {
        let files = ten_line_files();
        let session = ReviewSession {
            attention_regions: vec![
                region(
                    &files,
                    SalienceSource::Agent,
                    Salience::Skim,
                    Some(1),
                    Some(10),
                ),
                region(
                    &files,
                    SalienceSource::Agent,
                    Salience::Spotlight,
                    Some(5),
                    None,
                ),
                region(
                    &files,
                    SalienceSource::Human,
                    Salience::Supporting,
                    Some(4),
                    Some(6),
                ),
            ],
            ..Default::default()
        };
        let effective = list_effective_attention(&session, &files);
        for region in effective.iter().filter(|region| {
            region
                .target
                .line
                .is_some_and(|line| (4..=6).contains(&line))
        }) {
            assert_eq!(region.salience, Salience::Supporting);
            assert_eq!(region.source, Some(SalienceSource::Human));
        }
        assert!(!effective.iter().any(|region| {
            region.target.line == Some(5) && region.salience == Salience::Spotlight
        }));
    }

    #[test]
    fn effective_partition_uses_sparse_multi_hunk_rows_without_gap_windows() {
        let files = sparse_files();
        let session = ReviewSession {
            attention_regions: vec![
                region(
                    &files,
                    SalienceSource::Agent,
                    Salience::Skim,
                    Some(1),
                    Some(101),
                ),
                region(
                    &files,
                    SalienceSource::Agent,
                    Salience::Spotlight,
                    Some(100),
                    None,
                ),
            ],
            ..Default::default()
        };
        let spans = list_effective_attention(&session, &files)
            .into_iter()
            .filter(|region| region.target.line.is_some())
            .map(|region| {
                let start = region.target.line.unwrap();
                (
                    start,
                    region.target.end_line.unwrap_or(start),
                    region.salience,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            spans,
            [
                (1, 2, Salience::Skim),
                (100, 100, Salience::Spotlight),
                (101, 101, Salience::Skim),
            ]
        );
        assert!(
            spans
                .iter()
                .all(|(start, end, _)| *end <= 2 || *start >= 100),
            "effective output must not synthesize the unanchorable 3-99 gap"
        );
    }

    #[test]
    fn effective_partition_keeps_adjacent_assignment_boundaries_and_singletons() {
        let files = ten_line_files();
        let session = ReviewSession {
            attention_regions: vec![
                region(
                    &files,
                    SalienceSource::Agent,
                    Salience::Skim,
                    Some(1),
                    Some(2),
                ),
                region(
                    &files,
                    SalienceSource::Agent,
                    Salience::Spotlight,
                    Some(3),
                    Some(4),
                ),
                region(
                    &files,
                    SalienceSource::Human,
                    Salience::Supporting,
                    Some(7),
                    None,
                ),
            ],
            ..Default::default()
        };
        let spans = list_effective_attention(&session, &files)
            .into_iter()
            .filter(|region| region.target.line.is_some())
            .map(|region| {
                (
                    region.target.line.unwrap(),
                    region
                        .target
                        .end_line
                        .unwrap_or(region.target.line.unwrap()),
                    region.salience,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            spans,
            [
                (1, 2, Salience::Skim),
                (3, 4, Salience::Spotlight),
                (7, 7, Salience::Supporting),
            ]
        );
    }

    #[test]
    fn effective_partition_handles_usize_max_without_endpoint_overflow() {
        let files = vec![FileDiff {
            path: "src/lib.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            additions: 0,
            deletions: 0,
            raw: String::new(),
            fingerprint: "fingerprint".into(),
            hunks: vec![Hunk {
                old_start: usize::MAX,
                old_len: 1,
                new_start: usize::MAX,
                new_len: 1,
                header: "@@ defensive @@".into(),
                lines: vec![DiffLine {
                    kind: DiffLineKind::Context,
                    old_lineno: Some(usize::MAX),
                    new_lineno: Some(usize::MAX),
                    text: "last possible line".into(),
                }],
            }],
        }];
        let session = ReviewSession {
            attention_regions: vec![region(
                &files,
                SalienceSource::Human,
                Salience::Spotlight,
                Some(usize::MAX),
                None,
            )],
            ..Default::default()
        };
        let spans = list_effective_attention(&session, &files);
        let singleton = spans
            .iter()
            .find(|region| region.target.line == Some(usize::MAX))
            .expect("usize::MAX singleton must not be dropped");
        assert_eq!(singleton.target.end_line, None);
        assert_eq!(singleton.salience, Salience::Spotlight);
    }

    #[test]
    fn walkthrough_importance_maps_to_agent_salience() {
        let files = files();
        let mut session = ReviewSession::default();
        session.walkthroughs.push(crate::state::Walkthrough {
            steps: vec![crate::state::WalkthroughStep {
                target: target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap(),
                importance: StepImportance::Glance,
                why: Some("routine context".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        sync_agent_attention(&mut session, &files).unwrap();
        assert_eq!(session.attention_regions[0].source, SalienceSource::Agent);
        assert_eq!(session.attention_regions[0].salience, Salience::Skim);
    }

    #[test]
    fn agent_sync_retains_unreanchorable_sources_and_removes_only_deleted_sources() {
        let files = files();
        let step_target = target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap();
        let mut session = ReviewSession {
            walkthroughs: vec![crate::state::Walkthrough {
                steps: vec![crate::state::WalkthroughStep {
                    target: step_target,
                    why: Some("important".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        sync_agent_attention(&mut session, &files).unwrap();
        let assigned = session.attention_regions[0].clone();

        sync_agent_attention(&mut session, &[]).unwrap();
        assert_eq!(
            session.attention_regions.as_slice(),
            std::slice::from_ref(&assigned)
        );
        assert!(region_is_stale(&session.attention_regions[0], &[]));

        let only_line_one = DiffSet::parse(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap()
        .files;
        sync_agent_attention(&mut session, &only_line_one).unwrap();
        assert_eq!(session.attention_regions, [assigned]);

        let reanchored = target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap();
        session.walkthroughs[0].steps[0].target = reanchored.clone();
        sync_agent_attention(&mut session, &files).unwrap();
        assert_eq!(session.attention_regions[0].target, reanchored);
        assert!(!region_is_stale(&session.attention_regions[0], &files));

        session.walkthroughs[0].steps.clear();
        sync_agent_attention(&mut session, &only_line_one).unwrap();
        assert!(session.attention_regions.is_empty());
    }

    #[test]
    fn agent_rationale_uses_first_trimmed_non_empty_value_and_state_saves() {
        let files = files();
        let mut session = ReviewSession {
            id: "session".into(),
            walkthroughs: vec![crate::state::Walkthrough {
                steps: vec![
                    crate::state::WalkthroughStep {
                        target: target_for_diff(&files, "src/lib.rs", Some(1), None).unwrap(),
                        why: Some("  chosen why  ".into()),
                        body: Some("ignored body".into()),
                        title: Some("ignored title".into()),
                        ..Default::default()
                    },
                    crate::state::WalkthroughStep {
                        target: target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap(),
                        why: Some("  ".into()),
                        body: Some("\n meaningful body \t".into()),
                        title: Some("fallback title".into()),
                        ..Default::default()
                    },
                    crate::state::WalkthroughStep {
                        target: target_for_diff(&files, "src/lib.rs", Some(3), None).unwrap(),
                        why: Some(" ".into()),
                        body: Some("\n".into()),
                        title: Some("\t".into()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        sync_agent_attention(&mut session, &files).unwrap();
        assert_eq!(
            session.attention_regions[0].rationale.as_deref(),
            Some("chosen why")
        );
        assert_eq!(
            session.attention_regions[1].rationale.as_deref(),
            Some("meaningful body")
        );
        assert_eq!(session.attention_regions[2].rationale, None);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        crate::state::ReviewState {
            sessions: vec![session],
            ..Default::default()
        }
        .save(&path)
        .unwrap();
    }

    #[test]
    fn clear_uses_normalized_identity_without_anchor_or_current_diff() {
        let files = files();
        let mut session = ReviewSession {
            attention_regions: vec![region(
                &files,
                SalienceSource::Human,
                Salience::Skim,
                Some(2),
                Some(2),
            )],
            ..Default::default()
        };
        let identity = identity_target("src/lib.rs", Some(2), None).unwrap();
        assert!(clear_human_attention(&mut session, &identity));
        assert!(session.attention_regions.is_empty());
    }

    #[test]
    fn exact_clear_preserves_file_level_overlap_and_other_sources() {
        let files = files();
        let human_file = region(
            &files,
            SalienceSource::Human,
            Salience::Supporting,
            None,
            None,
        );
        let human_line = region(
            &files,
            SalienceSource::Human,
            Salience::Skim,
            Some(2),
            Some(2),
        );
        let agent_line = region(
            &files,
            SalienceSource::Agent,
            Salience::Spotlight,
            Some(2),
            None,
        );
        let mut session = ReviewSession {
            attention_regions: vec![human_file.clone(), human_line, agent_line.clone()],
            ..Default::default()
        };
        let identity = identity_target("src/lib.rs", Some(2), None).unwrap();
        assert!(clear_human_attention(&mut session, &identity));
        assert_eq!(session.attention_regions, [human_file, agent_line]);
    }

    #[test]
    fn promote_demote_boundaries_and_inherited_sources_create_human_overrides() {
        let files = files();
        let target = target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap();
        let mut session = ReviewSession::default();
        let promoted = promote_human_attention(&mut session, target.clone(), None, &files).unwrap();
        assert_eq!(promoted.salience, Salience::Spotlight);
        assert_eq!(promoted.source, SalienceSource::Human);
        let promoted_again =
            promote_human_attention(&mut session, target.clone(), None, &files).unwrap();
        assert_eq!(promoted_again.salience, Salience::Spotlight);

        clear_human_attention(&mut session, &target);
        session.attention_regions.push(AttentionRegion {
            target: target.clone(),
            salience: Salience::Skim,
            rationale: Some("heuristic".into()),
            source: SalienceSource::Heuristic,
        });
        let inherited =
            promote_human_attention(&mut session, target.clone(), None, &files).unwrap();
        assert_eq!(inherited.salience, Salience::Supporting);
        assert_eq!(inherited.source, SalienceSource::Human);

        clear_human_attention(&mut session, &target);
        session
            .attention_regions
            .retain(|region| region.source != SalienceSource::Heuristic);
        session.attention_regions.push(AttentionRegion {
            target: target.clone(),
            salience: Salience::Spotlight,
            rationale: Some("agent".into()),
            source: SalienceSource::Agent,
        });
        let inherited = demote_human_attention(&mut session, target.clone(), None, &files).unwrap();
        assert_eq!(inherited.salience, Salience::Supporting);
        assert_eq!(inherited.source, SalienceSource::Human);
        let demoted = demote_human_attention(&mut session, target.clone(), None, &files).unwrap();
        assert_eq!(demoted.salience, Salience::Skim);
        let demoted_again = demote_human_attention(&mut session, target, None, &files).unwrap();
        assert_eq!(demoted_again.salience, Salience::Skim);
    }

    #[test]
    fn heuristics_seed_only_noisy_files_and_preserve_drift() {
        let mut files = files();
        files[0].path = "Cargo.lock".into();
        let mut session = ReviewSession::default();
        let first = update_heuristic_attention(
            &mut session,
            &files,
            &GeneratedPolicy::default(),
            &[],
            false,
        )
        .unwrap();
        assert_eq!(first.added, 1);
        assert_eq!(session.attention_regions[0].salience, Salience::Skim);

        let mut changed = files.clone();
        changed[0].fingerprint = "drifted".into();
        let update = update_heuristic_attention(
            &mut session,
            &changed,
            &GeneratedPolicy::default(),
            &[],
            true,
        )
        .unwrap();
        assert_eq!(update.preserved_stale, 1);
        assert_eq!(session.attention_regions.len(), 1);
    }

    #[test]
    fn heuristic_recompute_never_removes_agent_assignments() {
        let mut files = files();
        files[0].path = "Cargo.lock".into();
        let target = target_for_diff(&files, "Cargo.lock", None, None).unwrap();
        let agent = AttentionRegion {
            target,
            salience: Salience::Spotlight,
            rationale: Some("agent curation".into()),
            source: SalienceSource::Agent,
        };
        let mut session = ReviewSession {
            attention_regions: vec![agent.clone()],
            walkthroughs: vec![crate::state::Walkthrough {
                steps: vec![crate::state::WalkthroughStep {
                    target: agent.target.clone(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        sync_agent_attention(&mut session, &[]).unwrap();
        update_heuristic_attention(&mut session, &[], &GeneratedPolicy::default(), &[], true)
            .unwrap();
        assert!(session.attention_regions.contains(&agent));
    }

    #[test]
    fn heuristic_recompute_never_removes_human_assignments() {
        let mut files = files();
        files[0].path = "Cargo.lock".into();
        let human = AttentionRegion {
            target: target_for_diff(&files, "Cargo.lock", None, None).unwrap(),
            salience: Salience::Supporting,
            rationale: Some("review this lockfile".into()),
            source: SalienceSource::Human,
        };
        let mut session = ReviewSession {
            attention_regions: vec![human.clone()],
            ..Default::default()
        };
        update_heuristic_attention(&mut session, &[], &GeneratedPolicy::default(), &[], true)
            .unwrap();
        assert_eq!(session.attention_regions, [human]);
    }
}
