//! Standalone loopback web peer.
//!
//! The browser is a renderer over the app-owned reading projection. This
//! durable state and jj changes are projected into surgical SSE patches.
//! The browser also reports ephemeral interaction state and renders the same
//! socket-driven presentation commands as the TUI. Durable browser mutations
//! are thin, generation-guarded adapters over the shared review services.

use std::{
    collections::BTreeMap,
    fs,
    future::IntoFuture,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    pin::Pin,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::{Receiver as BlockingReceiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    task::{Context as TaskContext, Poll},
    time::{Duration, SystemTime},
};

use axum::{
    Router,
    extract::{Json, Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{
        Html, IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use color_eyre::eyre::{Context, Result};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
};

use crate::{
    acp::socket::{AcpBridge, PresentCommand},
    app::{Focus, ReadingRegion, ReadingRegionKind, ReviewSession},
    attention::SkimSelection,
    config::ThemeConfig,
    diff::{DiffSet, FileDiff},
    generated::GeneratedMatcher,
    jj::{JjBackend, JjProcessControl},
    registry::{InstanceInfo, InstanceRegistration},
    review,
    state::{ActionIntent, Channel, CommentKind, CommentState, ReviewState, Salience},
    web_render::{
        self, COMPONENT_CSS, GuideView, PREPAINT_SCRIPT, RenderMode, RenderOptions,
        THEME_CONTROL_SCRIPT, script_hash_source,
    },
};

mod actions;
mod assets;
mod interactions;
mod presentation;
mod projection;
mod runtime;
mod server;
mod shared;

use actions::*;
use assets::*;
use interactions::*;
use presentation::*;
use projection::*;
pub(crate) use runtime::run;
use runtime::*;
use server::*;
pub(crate) use shared::WebParams;
use shared::*;

#[cfg(test)]
mod tests;
