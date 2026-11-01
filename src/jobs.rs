//! Background jobs with a live log. Every state-changing action runs as a job so the
//! UI can show a progress overlay with real-time output.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const MAX_LINES: usize = 2000;
const MAX_JOBS: usize = 30;

#[derive(Clone, Copy, Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Serialize)]
pub struct Job {
    pub id: u64,
    pub title: String,
    pub status: JobStatus,
    pub lines: Vec<String>,
    /// Number of lines dropped from the front of `lines` (so clients can track offsets).
    pub dropped: usize,
    pub error: Option<String>,
}

#[derive(Clone, Default)]
pub struct Jobs {
    inner: Arc<Mutex<HashMap<u64, Job>>>,
    counter: Arc<AtomicU64>,
}

impl Jobs {
    pub fn create(&self, title: &str) -> JobLog {
        let id = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let mut map = self.inner.lock().unwrap();
        if map.len() >= MAX_JOBS {
            if let Some(oldest) = map.values().filter(|j| matches!(j.status, JobStatus::Done | JobStatus::Failed)).map(|j| j.id).min() {
                map.remove(&oldest);
            }
        }
        map.insert(
            id,
            Job { id, title: title.to_string(), status: JobStatus::Queued, lines: vec![], dropped: 0, error: None },
        );
        JobLog { jobs: self.clone(), id }
    }

    pub fn get(&self, id: u64) -> Option<Job> {
        self.inner.lock().unwrap().get(&id).cloned()
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut Job)) {
        if let Some(j) = self.inner.lock().unwrap().get_mut(&id) {
            f(j);
        }
    }
}

/// Handle used by long-running operations to report progress.
#[derive(Clone)]
pub struct JobLog {
    jobs: Jobs,
    pub id: u64,
}

impl JobLog {
    pub fn push(&self, line: impl Into<String>) {
        let line = line.into();
        self.jobs.update(self.id, |j| {
            j.lines.push(line);
            if j.lines.len() > MAX_LINES {
                j.lines.remove(0);
                j.dropped += 1;
            }
        });
    }

    pub fn set_running(&self) {
        self.jobs.update(self.id, |j| j.status = JobStatus::Running);
    }

    pub fn finish(&self, error: Option<String>) {
        self.jobs.update(self.id, |j| {
            j.status = if error.is_some() { JobStatus::Failed } else { JobStatus::Done };
            j.error = error;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_and_cap() {
        let jobs = Jobs::default();
        let log = jobs.create("t");
        assert_eq!(jobs.get(log.id).unwrap().status, JobStatus::Queued);
        log.set_running();
        for i in 0..(MAX_LINES + 5) {
            log.push(format!("l{i}"));
        }
        let j = jobs.get(log.id).unwrap();
        assert_eq!(j.lines.len(), MAX_LINES);
        assert_eq!(j.dropped, 5);
        log.finish(Some("boom".into()));
        let j = jobs.get(log.id).unwrap();
        assert_eq!(j.status, JobStatus::Failed);
        assert_eq!(j.error.as_deref(), Some("boom"));
    }
}
