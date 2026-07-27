//! Toolkit-independent live-presentation target resolution.
//!
//! Presentation transports and renderers own busy-state arbitration, viewport
//! movement, notices, and browser broadcasts. This service owns the semantic
//! contract for resolving an ephemeral Focus request against the current
//! review stream, including its stable error taxonomy and response payload.

use std::fmt;

use serde::Serialize;

use crate::{anchor::CommentAnchor, app::ReviewSession};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PresentError {
    PathNotInDiff(String),
    InvalidRange,
    LocationNotInDiff { path: String, line: usize },
}

impl PresentError {
    pub(crate) fn code(&self) -> i64 {
        -32602
    }

    pub(crate) fn into_rpc(self) -> (i64, String) {
        (self.code(), self.to_string())
    }
}

impl fmt::Display for PresentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathNotInDiff(path) => write!(formatter, "path is not in the diff: {path}"),
            Self::InvalidRange => {
                formatter.write_str("end_line must be greater than or equal to line")
            }
            Self::LocationNotInDiff { path, line } => {
                write!(formatter, "location is not in the diff: {path}:{line}")
            }
        }
    }
}

/// A validated current-stream Focus destination and its protocol result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FocusTarget {
    pub(crate) stream_row: usize,
    pub(crate) status: FocusStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FocusStatus {
    ok: bool,
    path: String,
    line: usize,
    end_line: Option<usize>,
}

/// Validate and resolve a `present/focus` target in current diff line space.
///
/// A file must be part of the current diff and the requested inclusive range
/// must contain at least one anchorable stream row. Durable targets which no
/// longer satisfy that rule are stale and receive the same location error as
/// any other non-current line.
pub(crate) fn resolve_focus_target(
    session: &ReviewSession,
    path: &str,
    line: usize,
    end_line: Option<usize>,
) -> Result<FocusTarget, PresentError> {
    if !session.files.iter().any(|file| file.path == path) {
        return Err(PresentError::PathNotInDiff(path.to_owned()));
    }
    let requested_end = end_line.unwrap_or(line);
    if requested_end < line {
        return Err(PresentError::InvalidRange);
    }
    let stream_row = session
        .review_stream()
        .rows
        .iter()
        .enumerate()
        .find_map(|(index, row)| {
            if row.path.as_deref() != Some(path) {
                return None;
            }
            let anchor_line = row.anchor.as_ref().and_then(CommentAnchor::line)?;
            (line <= anchor_line && anchor_line <= requested_end).then_some(index)
        })
        .ok_or_else(|| PresentError::LocationNotInDiff {
            path: path.to_owned(),
            line,
        })?;

    Ok(FocusTarget {
        stream_row,
        status: FocusStatus {
            ok: true,
            path: path.to_owned(),
            line,
            end_line,
        },
    })
}
