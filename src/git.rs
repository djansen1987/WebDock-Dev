//! Thin wrapper around the `git` CLI: clone, pull and repository info for project folders.

use crate::error::{AppError, AppResult};
use crate::jobs::JobLog;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize)]
pub struct GitInfo {
    pub branch: Option<String>,
    /// URL of `origin` with any embedded credentials removed.
    pub remote: Option<String>,
}

pub fn available() -> bool {
    let mut cmd = std::process::Command::new("git");
    crate::proc::quiet(&mut cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn require_git() -> AppResult<()> {
    if available() {
        Ok(())
    } else {
        Err(AppError::BadRequest("Git is not installed. Install it from https://git-scm.com/ (or run `winget install Git.Git`) and restart WebDock Dev.".into()))
    }
}

/// Accepts https/http/ssh/scp-style URLs and the GitHub shorthand `owner/repo`; rejects everything else
/// (option injection such as `--upload-pack=...` and exotic transports such as `ext::`).
pub fn normalize_url(input: &str) -> AppResult<String> {
    let url = input.trim();
    if url.is_empty() {
        return Err(AppError::BadRequest("enter a Git repository URL".into()));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(AppError::BadRequest("the Git URL must not contain spaces".into()));
    }
    let shorthand = {
        let mut parts = url.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(a), Some(b), None) => {
                [a, b].iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)))
            }
            _ => false,
        }
    };
    if shorthand && !url.starts_with('-') {
        return Ok(format!("https://github.com/{}.git", url.trim_end_matches(".git")));
    }
    let ok = ["https://", "http://", "ssh://", "git@"].iter().any(|p| url.starts_with(p));
    if !ok {
        return Err(AppError::BadRequest("use an https:// or ssh URL (for example https://github.com/owner/repo.git) or owner/repo".into()));
    }
    Ok(url.to_string())
}

pub fn validate_branch(b: &str) -> AppResult<()> {
    let ok = !b.is_empty()
        && b.len() <= 100
        && !b.starts_with('-')
        && !b.contains("..")
        && b.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c));
    if ok {
        Ok(())
    } else {
        Err(AppError::BadRequest("branch: use letters, digits and - _ . /".into()))
    }
}

/// Repository name from a URL, usable as a project name (lowercase a-z, 0-9, '-').
pub fn name_from_url(url: &str) -> String {
    let last = url.trim_end_matches('/').rsplit(|c| c == '/' || c == ':').next().unwrap_or("");
    let base = last.trim_end_matches(".git");
    let mut out = String::new();
    for c in base.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(40).collect()
}

fn redact(url: &str) -> String {
    match (url.find("://"), url.find('@')) {
        (Some(s), Some(at)) if at > s + 3 && !url[s + 3..at].contains('/') => format!("{}{}", &url[..s + 3], &url[at + 1..]),
        _ => url.to_string(),
    }
}

fn git_cmd() -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("git");
    crate::proc::quiet_tokio(&mut cmd);
    cmd.env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).kill_on_drop(true);
    cmd
}

/// Runs git and streams its output (progress lines end in '\r') into the job log.
async fn run_streaming(mut cmd: tokio::process::Command, log: &JobLog, what: &str) -> AppResult<()> {
    use tokio::io::{AsyncReadExt, BufReader};

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| AppError::BadRequest(format!("could not run git: {e}")))?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>), child.stderr.take().map(|s| Box::new(s) as _)]
        .into_iter()
        .flatten()
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut r = BufReader::new(stream);
            let (mut buf, mut line) = ([0u8; 4096], Vec::new());
            while let Ok(n) = r.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                for &b in &buf[..n] {
                    if b == b'\n' || b == b'\r' {
                        if !line.is_empty() {
                            let _ = tx.send(String::from_utf8_lossy(&line).to_string());
                            line.clear();
                        }
                    } else {
                        line.push(b);
                    }
                }
            }
            if !line.is_empty() {
                let _ = tx.send(String::from_utf8_lossy(&line).to_string());
            }
        });
    }
    drop(tx);

    let mut tail = std::collections::VecDeque::new();
    let mut last_progress = Instant::now() - Duration::from_secs(1);
    while let Some(l) = rx.recv().await {
        if tail.len() == 8 {
            tail.pop_front();
        }
        tail.push_back(l.clone());
        // Progress updates arrive many times per second; throttle them.
        if !l.contains('%') || last_progress.elapsed() > Duration::from_millis(700) {
            log.push(l);
            last_progress = Instant::now();
        }
    }
    let status = child.wait().await.map_err(|e| AppError::BadRequest(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!("{what} failed: {}", tail.into_iter().collect::<Vec<_>>().join(" | "))))
    }
}

pub async fn clone(url: &str, branch: Option<&str>, dest: &Path, log: &JobLog) -> AppResult<()> {
    require_git()?;
    log.push(format!("==> Cloning {}", redact(url)));
    let mut cmd = git_cmd();
    cmd.args(["-c", "protocol.ext.allow=never", "clone", "--progress"]);
    if let Some(b) = branch {
        cmd.args(["--branch", b]);
    }
    cmd.arg("--").arg(url).arg(dest);
    run_streaming(cmd, log, "git clone").await
}

pub async fn pull(dir: &Path, log: &JobLog) -> AppResult<()> {
    require_git()?;
    log.push("Running git pull --ff-only ...");
    let mut cmd = git_cmd();
    cmd.arg("-C").arg(dir).args(["pull", "--ff-only"]);
    run_streaming(cmd, log, "git pull").await
}

async fn out(dir: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = git_cmd();
    cmd.arg("-C").arg(dir).args(args).stdout(Stdio::piped()).stderr(Stdio::null());
    let o = tokio::time::timeout(Duration::from_secs(5), cmd.output()).await.ok()?.ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

/// Branch and origin of the repository in `dir`; None when the folder is not a git work tree.
pub async fn info(dir: &Path) -> Option<GitInfo> {
    if !dir.join(".git").exists() {
        return None;
    }
    let branch = out(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await;
    let remote = out(dir, &["remote", "get-url", "origin"]).await.map(|u| redact(&u));
    Some(GitInfo { branch, remote })
}

static CACHE: Mutex<Option<HashMap<String, (Instant, Option<GitInfo>)>>> = Mutex::new(None);

/// `info` with a 30 second cache, because the dashboard polls the project list every few seconds.
pub async fn info_cached(dir: &Path) -> Option<GitInfo> {
    let key = dir.to_string_lossy().to_string();
    if let Some((at, v)) = CACHE.lock().unwrap().as_ref().and_then(|m| m.get(&key)) {
        if at.elapsed() < Duration::from_secs(30) {
            return v.clone();
        }
    }
    let v = info(dir).await;
    CACHE.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, (Instant::now(), v.clone()));
    v
}

pub fn forget(dir: &Path) {
    if let Some(m) = CACHE.lock().unwrap().as_mut() {
        m.remove(&dir.to_string_lossy().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(normalize_url("owner/repo").unwrap(), "https://github.com/owner/repo.git");
        assert!(normalize_url("https://github.com/a/b.git").is_ok());
        assert!(normalize_url("git@github.com:a/b.git").is_ok());
        assert!(normalize_url("--upload-pack=calc").is_err());
        assert!(normalize_url("ext::sh -c calc").is_err());
        assert!(normalize_url("file:///c/x").is_err());
    }

    #[test]
    fn names() {
        assert_eq!(name_from_url("https://github.com/WordPress/WordPress.git"), "wordpress");
        assert_eq!(name_from_url("git@github.com:me/My_Cool.Site.git"), "my-cool-site");
        assert_eq!(redact("https://user:tok@github.com/a/b.git"), "https://github.com/a/b.git");
    }
}
