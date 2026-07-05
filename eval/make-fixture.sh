#!/usr/bin/env bash
# Generates a fixture jj repo with a realistic 3-change stack for gander evaluation.
# Usage: make-fixture.sh <target-dir>
set -euo pipefail
DIR="$1"
export JJ_CONFIG=/dev/null
export JJ_USER="Eval Fixture"
export JJ_EMAIL="eval@example.com"
jj() { command jj --config 'user.name="Eval Fixture"' --config 'user.email="eval@example.com"' "$@"; }

rm -rf "$DIR"
mkdir -p "$DIR"
cd "$DIR"
jj git init >/dev/null

mkdir -p src tests

# ---------- base commit: small task-queue library ----------
cat > Cargo.toml <<'EOF'
[package]
name = "taskq"
version = "0.1.0"
edition = "2021"
EOF

cat > src/lib.rs <<'EOF'
pub mod config;
pub mod queue;
pub mod worker;

pub use config::Config;
pub use queue::{Job, Queue};
pub use worker::Worker;
EOF

cat > src/config.rs <<'EOF'
/// Runtime configuration for the queue.
#[derive(Debug, Clone)]
pub struct Config {
    pub max_jobs: usize,
    pub max_retries: u32,
    pub poll_interval_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_jobs: 128,
            max_retries: 3,
            poll_interval_ms: 250,
        }
    }
}
EOF

cat > src/queue.rs <<'EOF'
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub payload: String,
}

#[derive(Debug, Default)]
pub struct Queue {
    jobs: VecDeque<Job>,
    next_id: u64,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enqueue(&mut self, payload: impl Into<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.jobs.push_back(Job { id, payload: payload.into() });
        id
    }

    pub fn dequeue(&mut self) -> Option<Job> {
        self.jobs.pop_front()
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}
EOF

cat > src/worker.rs <<'EOF'
use crate::config::Config;
use crate::queue::{Job, Queue};

pub struct Worker {
    config: Config,
    processed: u64,
}

impl Worker {
    pub fn new(config: Config) -> Self {
        Self { config, processed: 0 }
    }

    /// Drain the queue, processing every job once. Failed jobs are dropped.
    pub fn run(&mut self, queue: &mut Queue) -> u64 {
        while let Some(job) = queue.dequeue() {
            if self.process(&job).is_ok() {
                self.processed += 1;
            }
        }
        self.processed
    }

    fn process(&self, job: &Job) -> Result<(), String> {
        if job.payload.is_empty() {
            return Err(format!("job {} has empty payload", job.id));
        }
        // pretend to do work
        Ok(())
    }

    pub fn processed(&self) -> u64 {
        self.processed
    }
}
EOF

cat > tests/basic.rs <<'EOF'
use taskq::{Config, Queue, Worker};

#[test]
fn processes_jobs_in_order() {
    let mut q = Queue::new();
    q.enqueue("a");
    q.enqueue("b");
    let mut w = Worker::new(Config::default());
    assert_eq!(w.run(&mut q), 2);
    assert!(q.is_empty());
}
EOF

jj describe -m "chore: initial task queue library" >/dev/null
jj bookmark create main -r @ >/dev/null 2>&1
jj new >/dev/null

# ---------- change 1: feat: priority scheduling ----------
cat > src/priority.rs <<'EOF'
/// Job priority. Higher priorities are dequeued first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Low,
    Normal,
    High,
    Critical,
}

impl Default for Priority {
    fn default() -> Self {
        Priority::Normal
    }
}

impl Priority {
    pub fn as_str(&self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Critical => "critical",
        }
    }
}
EOF

cat > src/queue.rs <<'EOF'
use std::collections::BTreeMap;
use std::collections::VecDeque;

use crate::priority::Priority;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub payload: String,
    pub priority: Priority,
}

/// Priority queue: jobs are dequeued highest-priority-first, FIFO within
/// a priority band.
#[derive(Debug, Default)]
pub struct Queue {
    bands: BTreeMap<Priority, VecDeque<Job>>,
    next_id: u64,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enqueue(&mut self, payload: impl Into<String>) -> u64 {
        self.enqueue_with_priority(payload, Priority::Normal)
    }

    pub fn enqueue_with_priority(
        &mut self,
        payload: impl Into<String>,
        priority: Priority,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.bands
            .entry(priority)
            .or_default()
            .push_back(Job { id, payload: payload.into(), priority });
        id
    }

    pub fn dequeue(&mut self) -> Option<Job> {
        // BTreeMap iterates ascending; take from the last (highest) band.
        let (&priority, _) = self.bands.iter().rev().find(|(_, q)| !q.is_empty())?;
        let job = self.bands.get_mut(&priority)?.pop_front();
        job
    }

    pub fn len(&self) -> usize {
        self.bands.values().map(|q| q.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
EOF

cat > src/lib.rs <<'EOF'
pub mod config;
pub mod priority;
pub mod queue;
pub mod worker;

pub use config::Config;
pub use priority::Priority;
pub use queue::{Job, Queue};
pub use worker::Worker;
EOF

jj describe -m "feat: priority scheduling for the job queue

Jobs now carry a Priority (Low/Normal/High/Critical). The queue keeps a
band per priority and dequeues highest-first, FIFO within a band." >/dev/null
jj new >/dev/null

# ---------- change 2: refactor: extract retry logic (contains a subtle bug) ----------
cat > src/retry.rs <<'EOF'
use crate::config::Config;

/// Tracks retry attempts for failing jobs.
#[derive(Debug, Default)]
pub struct RetryPolicy {
    max_retries: u32,
}

impl RetryPolicy {
    pub fn from_config(config: &Config) -> Self {
        Self { max_retries: config.max_retries }
    }

    /// Returns true if a job that has already been attempted `attempts`
    /// times should be retried again.
    pub fn should_retry(&self, attempts: u32) -> bool {
        // BUG (intentional for review): off-by-one. A job attempted
        // `max_retries` times gets retried once more, so max_retries=3
        // yields 4 retries total.
        attempts <= self.max_retries
    }
}
EOF

cat > src/worker.rs <<'EOF'
use crate::config::Config;
use crate::queue::{Job, Queue};
use crate::retry::RetryPolicy;

pub struct Worker {
    retry: RetryPolicy,
    processed: u64,
    dropped: u64,
}

impl Worker {
    pub fn new(config: Config) -> Self {
        Self {
            retry: RetryPolicy::from_config(&config),
            processed: 0,
            dropped: 0,
        }
    }

    /// Drain the queue. Failed jobs are re-enqueued until the retry policy
    /// gives up on them, then counted as dropped.
    pub fn run(&mut self, queue: &mut Queue) -> u64 {
        let mut attempts: std::collections::HashMap<u64, u32> =
            std::collections::HashMap::new();
        while let Some(job) = queue.dequeue() {
            match self.process(&job) {
                Ok(()) => {
                    self.processed += 1;
                }
                Err(_) => {
                    let n = attempts.entry(job.id).or_insert(0);
                    *n += 1;
                    if self.retry.should_retry(*n) {
                        queue.enqueue_with_priority(job.payload.clone(), job.priority);
                    } else {
                        self.dropped += 1;
                    }
                }
            }
        }
        self.processed
    }

    fn process(&self, job: &Job) -> Result<(), String> {
        if job.payload.is_empty() {
            return Err(format!("job {} has empty payload", job.id));
        }
        Ok(())
    }

    pub fn processed(&self) -> u64 {
        self.processed
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}
EOF

cat > src/lib.rs <<'EOF'
pub mod config;
pub mod priority;
pub mod queue;
pub mod retry;
pub mod worker;

pub use config::Config;
pub use priority::Priority;
pub use queue::{Job, Queue};
pub use worker::Worker;
EOF

jj describe -m "refactor: extract retry policy from worker

Failed jobs are now re-enqueued according to a RetryPolicy derived from
Config.max_retries instead of being silently dropped." >/dev/null
jj new >/dev/null

# ---------- change 3: fix + tests ----------
cat > tests/basic.rs <<'EOF'
use taskq::{Config, Priority, Queue, Worker};

#[test]
fn processes_jobs_in_order() {
    let mut q = Queue::new();
    q.enqueue("a");
    q.enqueue("b");
    let mut w = Worker::new(Config::default());
    assert_eq!(w.run(&mut q), 2);
    assert!(q.is_empty());
}

#[test]
fn high_priority_jobs_first() {
    let mut q = Queue::new();
    q.enqueue_with_priority("low", Priority::Low);
    q.enqueue_with_priority("critical", Priority::Critical);
    let first = q.dequeue().unwrap();
    assert_eq!(first.payload, "critical");
}

#[test]
fn empty_payload_jobs_are_dropped() {
    let mut q = Queue::new();
    q.enqueue("");
    let mut w = Worker::new(Config::default());
    w.run(&mut q);
    assert_eq!(w.dropped(), 1);
}
EOF

cat > src/config.rs <<'EOF'
/// Runtime configuration for the queue.
#[derive(Debug, Clone)]
pub struct Config {
    pub max_jobs: usize,
    pub max_retries: u32,
    pub poll_interval_ms: u64,
}

impl Config {
    /// Configuration suitable for tests: fast polling, one retry.
    pub fn for_tests() -> Self {
        Self {
            max_jobs: 8,
            max_retries: 1,
            poll_interval_ms: 1,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_jobs: 128,
            max_retries: 3,
            poll_interval_ms: 250,
        }
    }
}
EOF

jj describe -m "test: cover priority ordering and retry drops

Adds Config::for_tests() and integration coverage for priority ordering
and dropped-job accounting." >/dev/null
jj new >/dev/null

jj log --no-pager -r 'all()' -n 8
echo "FIXTURE OK: $DIR"
