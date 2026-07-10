//! Portable evidence for the code visible when comments and replies are made.
//!
//! Provenance is deliberately derived from an already-loaded diff. It contains
//! no jj operation/change identifiers and never performs a backend query.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    anchor::CommentAnchor,
    diff::{DiffLineKind, FileDiff, FileStatus},
    state::ReviewTarget,
};

pub const REVIEW_SCOPE_FINGERPRINT_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotEvidence {
    pub captured_at: DateTime<Utc>,
    pub identity: SnapshotIdentity,
    pub scope: ReviewScopeFingerprint,
    pub files: Vec<SnapshotFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotIdentity {
    pub session_id: String,
    pub target: ReviewTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewScopeFingerprint {
    pub version: u8,
    pub aggregate: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: FileStatus,
    /// SHA-256 of the exact existing raw per-file diff.
    pub diff_fingerprint: String,
    /// SHA-256 of ordered parsed diff-line kind and text only. Null for binary
    /// or otherwise non-comparable patches with no parsed lines.
    pub portable_patch_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommentObservation {
    pub snapshot: SnapshotEvidence,
    /// The original anchor, before edits or refresh re-anchoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<CommentAnchor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommentReplyResult {
    pub parent_comment_id: String,
    /// Null for comments created before observation capture existed.
    pub observation_aggregate_fingerprint: Option<String>,
    pub snapshot: SnapshotEvidence,
    pub related: RelatedTransition,
    /// Null when either side has no related file to compare.
    pub portable_patch_changed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RelatedTransition {
    SamePath { path: String },
    RenamedFrom { old_path: String, path: String },
    NotInDiff { path: Option<String> },
}

impl SnapshotEvidence {
    pub fn capture<'a>(
        captured_at: DateTime<Utc>,
        session_id: impl Into<String>,
        target: ReviewTarget,
        files: impl IntoIterator<Item = &'a FileDiff>,
    ) -> Self {
        let mut files = files
            .into_iter()
            .map(SnapshotFile::from)
            .collect::<Vec<_>>();
        files.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.old_path.cmp(&right.old_path))
                .then_with(|| file_status_tag(left.status).cmp(&file_status_tag(right.status)))
        });
        let aggregate = aggregate_fingerprint(&files);
        Self {
            captured_at,
            identity: SnapshotIdentity {
                session_id: session_id.into(),
                target,
            },
            scope: ReviewScopeFingerprint {
                version: REVIEW_SCOPE_FINGERPRINT_VERSION,
                aggregate,
            },
            files,
        }
    }
}

impl CommentObservation {
    pub fn new(snapshot: SnapshotEvidence, anchor: Option<CommentAnchor>) -> Self {
        Self { snapshot, anchor }
    }
}

impl CommentReplyResult {
    pub fn compare(
        parent_comment_id: impl Into<String>,
        observation: Option<&CommentObservation>,
        parent_path: Option<&str>,
        snapshot: SnapshotEvidence,
    ) -> Self {
        // An observation with no anchor is a captured general comment. Its
        // later mutable parent location must not rewrite what A represented.
        let original_path = match observation {
            Some(observation) => observation.anchor.as_ref().map(CommentAnchor::path),
            None => parent_path,
        };
        let observed_file = observation.and_then(|observation| {
            let path = observation.anchor.as_ref()?.path();
            observation
                .snapshot
                .files
                .iter()
                .find(|file| file.path == path)
        });
        let current_file = original_path.and_then(|path| {
            snapshot
                .files
                .iter()
                .find(|file| file.path == path)
                .map(|file| (file, false))
                .or_else(|| {
                    snapshot
                        .files
                        .iter()
                        .find(|file| {
                            file.status == FileStatus::Renamed
                                && file.old_path.as_deref() == Some(path)
                        })
                        .map(|file| (file, true))
                })
                .or_else(|| {
                    let observed =
                        observed_file.filter(|file| file.status == FileStatus::Renamed)?;
                    let lineage = observed.old_path.as_deref()?;
                    snapshot
                        .files
                        .iter()
                        .find(|file| {
                            file.status == FileStatus::Renamed
                                && file.old_path.as_deref() == Some(lineage)
                        })
                        .or_else(|| snapshot.files.iter().find(|file| file.path == lineage))
                        .map(|file| (file, true))
                })
        });
        let related = match (original_path, current_file) {
            (Some(old_path), Some((file, true))) => RelatedTransition::RenamedFrom {
                old_path: old_path.to_owned(),
                path: file.path.clone(),
            },
            (Some(path), Some((_, false))) => RelatedTransition::SamePath {
                path: path.to_owned(),
            },
            (path, None) => RelatedTransition::NotInDiff {
                path: path.map(str::to_owned),
            },
            (None, Some(_)) => unreachable!("a related file requires an original path"),
        };
        let portable_patch_changed = observation.and_then(|observation| {
            let path = original_path?;
            let original_file = observed_file.filter(|file| file.path == path).or_else(|| {
                observation
                    .snapshot
                    .files
                    .iter()
                    .find(|file| file.path == path)
            })?;
            let (current_file, _) = current_file?;
            Some(
                original_file.portable_patch_fingerprint.as_ref()?
                    != current_file.portable_patch_fingerprint.as_ref()?,
            )
        });
        Self {
            parent_comment_id: parent_comment_id.into(),
            observation_aggregate_fingerprint: observation
                .map(|observation| observation.snapshot.scope.aggregate.clone()),
            snapshot,
            related,
            portable_patch_changed,
        }
    }
}

impl From<&FileDiff> for SnapshotFile {
    fn from(file: &FileDiff) -> Self {
        Self {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            status: file.status,
            diff_fingerprint: file.fingerprint.clone(),
            portable_patch_fingerprint: portable_patch_fingerprint(file),
        }
    }
}

pub fn portable_patch_fingerprint(file: &FileDiff) -> Option<String> {
    if file.is_binary() || file.hunks.is_empty() {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"gander-portable-patch-v1\0");
    for line in file.hunks.iter().flat_map(|hunk| &hunk.lines) {
        digest.update([diff_line_kind_tag(line.kind)]);
        update_len_prefixed(&mut digest, line.text.as_bytes());
    }
    Some(format!("{:x}", digest.finalize()))
}

fn aggregate_fingerprint(files: &[SnapshotFile]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"gander-review-scope-v1\0");
    for file in files {
        update_len_prefixed(&mut digest, file.path.as_bytes());
        update_optional(&mut digest, file.old_path.as_deref());
        digest.update([file_status_tag(file.status)]);
        update_len_prefixed(&mut digest, file.diff_fingerprint.as_bytes());
        update_optional(&mut digest, file.portable_patch_fingerprint.as_deref());
    }
    format!("{:x}", digest.finalize())
}

fn update_optional(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            update_len_prefixed(digest, value.as_bytes());
        }
        None => digest.update([0]),
    }
}

fn update_len_prefixed(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn diff_line_kind_tag(kind: DiffLineKind) -> u8 {
    match kind {
        DiffLineKind::Context => 0,
        DiffLineKind::Added => 1,
        DiffLineKind::Removed => 2,
        DiffLineKind::Meta => 3,
    }
}

fn file_status_tag(status: FileStatus) -> u8 {
    match status {
        FileStatus::Added => 0,
        FileStatus::Modified => 1,
        FileStatus::Deleted => 2,
        FileStatus::Renamed => 3,
        FileStatus::Copied => 4,
        FileStatus::Binary => 5,
        FileStatus::Unknown => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffSet;

    fn one(raw: &str) -> FileDiff {
        DiffSet::parse(raw).unwrap().files.remove(0)
    }

    #[test]
    fn portable_fingerprint_ignores_headers_and_coordinates() {
        let a = one("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new");
        let b = one(
            "diff --git a/renamed.rs b/renamed.rs\n--- a/renamed.rs\n+++ b/renamed.rs\n@@ -99 +120 @@ function\n-old\n+new",
        );
        assert_ne!(a.fingerprint, b.fingerprint);
        assert_eq!(
            portable_patch_fingerprint(&a),
            portable_patch_fingerprint(&b)
        );
    }

    #[test]
    fn portable_fingerprint_changes_with_line_kind_or_text() {
        let base = one("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new");
        let text = one("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+different");
        let kind = one("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n old\n-new");
        assert_ne!(
            portable_patch_fingerprint(&base),
            portable_patch_fingerprint(&text)
        );
        assert_ne!(
            portable_patch_fingerprint(&base),
            portable_patch_fingerprint(&kind)
        );
    }

    #[test]
    fn aggregate_is_deterministic_across_file_order() {
        let diff = DiffSet::parse("diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-m\n+n").unwrap();
        let mut reversed = diff.files.clone();
        reversed.reverse();
        let target = ReviewTarget::default();
        let a = SnapshotEvidence::capture(Utc::now(), "s", target.clone(), diff.files.iter());
        let b = SnapshotEvidence::capture(Utc::now(), "s", target, reversed.iter());
        assert_eq!(a.scope, b.scope);
        assert_eq!(a.files, b.files);
    }

    #[test]
    fn reply_result_tracks_same_path_rename_and_missing_file() {
        let original = DiffSet::parse(
            "diff --git a/old.rs b/old.rs\n--- a/old.rs\n+++ b/old.rs\n@@ -1 +1 @@\n-a\n+b",
        )
        .unwrap();
        let a = SnapshotEvidence::capture(
            Utc::now(),
            "s",
            ReviewTarget::default(),
            original.files.iter(),
        );
        let observation = CommentObservation::new(
            a,
            Some(CommentAnchor::File {
                path: "old.rs".into(),
                old_path: None,
                diff_fingerprint: original.files[0].fingerprint.clone(),
            }),
        );

        let same = SnapshotEvidence::capture(
            Utc::now(),
            "s",
            ReviewTarget::default(),
            original.files.iter(),
        );
        let result = CommentReplyResult::compare("c", Some(&observation), Some("old.rs"), same);
        assert_eq!(
            result.related,
            RelatedTransition::SamePath {
                path: "old.rs".into()
            }
        );
        assert_eq!(result.portable_patch_changed, Some(false));

        let renamed = DiffSet::parse("diff --git a/old.rs b/new.rs\nsimilarity index 50%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-a\n+c").unwrap();
        let b = SnapshotEvidence::capture(
            Utc::now(),
            "s",
            ReviewTarget::default(),
            renamed.files.iter(),
        );
        let result = CommentReplyResult::compare("c", Some(&observation), Some("old.rs"), b);
        assert_eq!(
            result.related,
            RelatedTransition::RenamedFrom {
                old_path: "old.rs".into(),
                path: "new.rs".into()
            }
        );
        assert_eq!(result.portable_patch_changed, Some(true));

        let empty: Vec<FileDiff> = Vec::new();
        let b = SnapshotEvidence::capture(Utc::now(), "s", ReviewTarget::default(), empty.iter());
        let result = CommentReplyResult::compare("c", Some(&observation), Some("old.rs"), b);
        assert_eq!(
            result.related,
            RelatedTransition::NotInDiff {
                path: Some("old.rs".into())
            }
        );
        assert_eq!(result.portable_patch_changed, None);

        let general_snapshot = SnapshotEvidence::capture(
            Utc::now(),
            "s",
            ReviewTarget::default(),
            original.files.iter(),
        );
        let general = CommentObservation::new(general_snapshot, None);
        let b = SnapshotEvidence::capture(
            Utc::now(),
            "s",
            ReviewTarget::default(),
            original.files.iter(),
        );
        let result =
            CommentReplyResult::compare("general", Some(&general), Some("later-location.rs"), b);
        assert_eq!(result.related, RelatedTransition::NotInDiff { path: None });
        assert!(result.observation_aggregate_fingerprint.is_some());
        assert_eq!(result.portable_patch_changed, None);
    }

    #[test]
    fn successive_full_diff_renames_follow_shared_old_path_lineage() {
        let a = DiffSet::parse(
            "diff --git a/old.rs b/mid.rs\nsimilarity index 50%\nrename from old.rs\nrename to mid.rs\n--- a/old.rs\n+++ b/mid.rs\n@@ -1 +1 @@\n-old\n+mid",
        )
        .unwrap();
        let observation = CommentObservation::new(
            SnapshotEvidence::capture(Utc::now(), "s", ReviewTarget::default(), a.files.iter()),
            Some(CommentAnchor::File {
                path: "mid.rs".into(),
                old_path: Some("old.rs".into()),
                diff_fingerprint: a.files[0].fingerprint.clone(),
            }),
        );
        let b = DiffSet::parse(
            "diff --git a/old.rs b/new.rs\nsimilarity index 50%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-old\n+new",
        )
        .unwrap();
        let result = CommentReplyResult::compare(
            "c",
            Some(&observation),
            Some("mid.rs"),
            SnapshotEvidence::capture(Utc::now(), "s", ReviewTarget::default(), b.files.iter()),
        );
        assert_eq!(
            result.related,
            RelatedTransition::RenamedFrom {
                old_path: "mid.rs".into(),
                path: "new.rs".into()
            }
        );

        let back = DiffSet::parse(
            "diff --git a/old.rs b/old.rs\n--- a/old.rs\n+++ b/old.rs\n@@ -1 +1 @@\n-old\n+back",
        )
        .unwrap();
        let result = CommentReplyResult::compare(
            "c",
            Some(&observation),
            Some("mid.rs"),
            SnapshotEvidence::capture(Utc::now(), "s", ReviewTarget::default(), back.files.iter()),
        );
        assert_eq!(
            result.related,
            RelatedTransition::RenamedFrom {
                old_path: "mid.rs".into(),
                path: "old.rs".into()
            }
        );
    }

    #[test]
    fn copy_is_not_a_rename_and_multiple_renames_choose_sorted_path() {
        let original = DiffSet::parse(
            "diff --git a/old.rs b/old.rs\n--- a/old.rs\n+++ b/old.rs\n@@ -1 +1 @@\n-a\n+b",
        )
        .unwrap();
        let observation = CommentObservation::new(
            SnapshotEvidence::capture(
                Utc::now(),
                "s",
                ReviewTarget::default(),
                original.files.iter(),
            ),
            Some(CommentAnchor::File {
                path: "old.rs".into(),
                old_path: None,
                diff_fingerprint: original.files[0].fingerprint.clone(),
            }),
        );
        let copied = DiffSet::parse(
            "diff --git a/old.rs b/copied.rs\nsimilarity index 100%\ncopy from old.rs\ncopy to copied.rs",
        )
        .unwrap();
        let result = CommentReplyResult::compare(
            "c",
            Some(&observation),
            Some("old.rs"),
            SnapshotEvidence::capture(
                Utc::now(),
                "s",
                ReviewTarget::default(),
                copied.files.iter(),
            ),
        );
        assert_eq!(
            result.related,
            RelatedTransition::NotInDiff {
                path: Some("old.rs".into())
            }
        );

        let multiple = DiffSet::parse(
            "diff --git a/old.rs b/z.rs\nsimilarity index 100%\nrename from old.rs\nrename to z.rs\ndiff --git a/old.rs b/a.rs\nsimilarity index 100%\nrename from old.rs\nrename to a.rs",
        )
        .unwrap();
        let result = CommentReplyResult::compare(
            "c",
            Some(&observation),
            Some("old.rs"),
            SnapshotEvidence::capture(
                Utc::now(),
                "s",
                ReviewTarget::default(),
                multiple.files.iter(),
            ),
        );
        assert_eq!(
            result.related,
            RelatedTransition::RenamedFrom {
                old_path: "old.rs".into(),
                path: "a.rs".into()
            }
        );

        let copied_a = DiffSet::parse(
            "diff --git a/old.rs b/mid.rs\nsimilarity index 100%\ncopy from old.rs\ncopy to mid.rs",
        )
        .unwrap();
        let copied_observation = CommentObservation::new(
            SnapshotEvidence::capture(
                Utc::now(),
                "s",
                ReviewTarget::default(),
                copied_a.files.iter(),
            ),
            Some(CommentAnchor::File {
                path: "mid.rs".into(),
                old_path: Some("old.rs".into()),
                diff_fingerprint: copied_a.files[0].fingerprint.clone(),
            }),
        );
        let result = CommentReplyResult::compare(
            "c",
            Some(&copied_observation),
            Some("mid.rs"),
            SnapshotEvidence::capture(
                Utc::now(),
                "s",
                ReviewTarget::default(),
                multiple.files.iter(),
            ),
        );
        assert_eq!(
            result.related,
            RelatedTransition::NotInDiff {
                path: Some("mid.rs".into())
            }
        );
    }

    #[test]
    fn binary_patches_are_not_portably_comparable() {
        let a = DiffSet::parse(
            "diff --git a/image.bin b/image.bin\nBinary files a/image.bin and b/image.bin differ",
        )
        .unwrap();
        let b = DiffSet::parse(
            "diff --git a/image.bin b/image.bin\nindex 111..222\nBinary files a/image.bin and b/image.bin differ",
        )
        .unwrap();
        assert_ne!(a.files[0].fingerprint, b.files[0].fingerprint);
        assert_eq!(portable_patch_fingerprint(&a.files[0]), None);
        assert_eq!(portable_patch_fingerprint(&b.files[0]), None);
        let observation = CommentObservation::new(
            SnapshotEvidence::capture(Utc::now(), "s", ReviewTarget::default(), a.files.iter()),
            Some(CommentAnchor::File {
                path: "image.bin".into(),
                old_path: None,
                diff_fingerprint: a.files[0].fingerprint.clone(),
            }),
        );
        let result = CommentReplyResult::compare(
            "c",
            Some(&observation),
            Some("image.bin"),
            SnapshotEvidence::capture(Utc::now(), "s", ReviewTarget::default(), b.files.iter()),
        );
        assert_eq!(result.portable_patch_changed, None);
        assert_eq!(result.snapshot.files[0].portable_patch_fingerprint, None);
    }

    #[test]
    fn binary_rename_relates_but_binary_copy_does_not() {
        let a = DiffSet::parse(
            "diff --git a/old.bin b/mid.bin\nsimilarity index 50%\nrename from old.bin\nrename to mid.bin\nBinary files a/old.bin and b/mid.bin differ",
        )
        .unwrap();
        let observation = CommentObservation::new(
            SnapshotEvidence::capture(Utc::now(), "s", ReviewTarget::default(), a.files.iter()),
            Some(CommentAnchor::File {
                path: "mid.bin".into(),
                old_path: Some("old.bin".into()),
                diff_fingerprint: a.files[0].fingerprint.clone(),
            }),
        );
        let renamed = DiffSet::parse(
            "diff --git a/old.bin b/new.bin\nsimilarity index 50%\nrename from old.bin\nrename to new.bin\nBinary files a/old.bin and b/new.bin differ",
        )
        .unwrap();
        let result = CommentReplyResult::compare(
            "c",
            Some(&observation),
            Some("mid.bin"),
            SnapshotEvidence::capture(
                Utc::now(),
                "s",
                ReviewTarget::default(),
                renamed.files.iter(),
            ),
        );
        assert_eq!(
            result.related,
            RelatedTransition::RenamedFrom {
                old_path: "mid.bin".into(),
                path: "new.bin".into()
            }
        );
        assert_eq!(result.portable_patch_changed, None);

        let copied = DiffSet::parse(
            "diff --git a/old.bin b/copy.bin\nsimilarity index 50%\ncopy from old.bin\ncopy to copy.bin\nBinary files a/old.bin and b/copy.bin differ",
        )
        .unwrap();
        let result = CommentReplyResult::compare(
            "c",
            Some(&observation),
            Some("mid.bin"),
            SnapshotEvidence::capture(
                Utc::now(),
                "s",
                ReviewTarget::default(),
                copied.files.iter(),
            ),
        );
        assert_eq!(
            result.related,
            RelatedTransition::NotInDiff {
                path: Some("mid.bin".into())
            }
        );
        assert_eq!(result.portable_patch_changed, None);
    }

    #[test]
    fn serde_shape_uses_documented_names_types_and_nulls() {
        let binary = DiffSet::parse(
            "diff --git a/image.bin b/image.bin\nBinary files a/image.bin and b/image.bin differ",
        )
        .unwrap();
        let snapshot = SnapshotEvidence::capture(
            chrono::DateTime::UNIX_EPOCH,
            "session",
            ReviewTarget {
                base: Some("trunk()".into()),
                revision: Some("@".into()),
                repo: Some("/repo".into()),
                ..Default::default()
            },
            binary.files.iter(),
        );
        let result = CommentReplyResult::compare("comment", None, Some("image.bin"), snapshot);
        let value = serde_json::to_value(result).unwrap();

        assert_eq!(
            value["observation_aggregate_fingerprint"],
            serde_json::Value::Null
        );
        assert_eq!(value["portable_patch_changed"], serde_json::Value::Null);
        assert_eq!(
            value["snapshot"]["files"][0]["portable_patch_fingerprint"],
            serde_json::Value::Null
        );
        assert_eq!(value["snapshot"]["files"][0]["status"], "Binary");
        assert_eq!(value["related"]["kind"], "same_path");
        assert_eq!(
            value["snapshot"]["identity"]["target"]["revset"],
            serde_json::Value::Null
        );
        assert!(
            value["snapshot"]["identity"]["target"]
                .get("symbol")
                .is_some()
        );
    }
}
