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
    pub upstream: Option<String>,
    /// Commits not yet pushed.
    pub ahead: u32,
    /// Commits on the upstream that are not pulled yet.
    pub behind: u32,
    /// Number of files with uncommitted changes (including untracked).
    pub changed: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FileChange {
    pub path: String,
    /// Status in the index (first porcelain column), ' ' when unchanged.
    pub index: char,
    /// Status in the work tree (second porcelain column).
    pub worktree: char,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Status {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<FileChange>,
}

/// Parses `git status --porcelain=v1 -b -z` output.
fn parse_status(raw: &str) -> Status {
    let mut st = Status::default();
    let mut parts = raw.split('\0');
    while let Some(entry) = parts.next() {
        if let Some(head) = entry.strip_prefix("## ") {
            let (names, counts) = match head.find(" [") {
                Some(i) => (&head[..i], Some(head[i + 2..].trim_end_matches(']'))),
                None => (head, None),
            };
            if let Some(rest) = names.strip_prefix("No commits yet on ") {
                st.branch = Some(rest.to_string());
            } else if names.starts_with("HEAD (no branch)") {
                st.branch = None;
            } else if let Some((b, u)) = names.split_once("...") {
                st.branch = Some(b.to_string());
                st.upstream = Some(u.to_string());
            } else {
                st.branch = Some(names.to_string());
            }
            for c in counts.unwrap_or("").split(", ") {
                if let Some(n) = c.strip_prefix("ahead ") {
                    st.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = c.strip_prefix("behind ") {
                    st.behind = n.parse().unwrap_or(0);
                }
            }
        } else if entry.len() > 3 {
            let mut ch = entry.chars();
            let (index, worktree) = (ch.next().unwrap(), ch.next().unwrap());
            st.files.push(FileChange { path: entry[3..].to_string(), index, worktree });
            // A rename/copy entry is followed by the original path.
            if index == 'R' || index == 'C' {
                parts.next();
            }
        }
    }
    st
}

pub async fn status(dir: &Path) -> AppResult<Status> {
    require_git()?;
    let mut cmd = git_cmd();
    cmd.arg("-C").arg(dir).args(["status", "--porcelain=v1", "-b", "-z", "-uall"]).stdout(Stdio::piped()).stderr(Stdio::piped());
    let o = tokio::time::timeout(Duration::from_secs(20), cmd.output())
        .await
        .map_err(|_| AppError::BadRequest("git status timed out".into()))?
        .map_err(|e| AppError::BadRequest(format!("could not run git: {e}")))?;
    if !o.status.success() {
        return Err(AppError::BadRequest(format!("git status failed: {}", String::from_utf8_lossy(&o.stderr).trim())));
    }
    Ok(parse_status(&String::from_utf8_lossy(&o.stdout)))
}

/// Rejects paths that could escape the repository or be read as git options.
fn validate_rel_path(p: &str) -> AppResult<()> {
    let bad = p.is_empty()
        || p.starts_with('-')
        || p.contains('\0')
        || Path::new(p).is_absolute()
        || p.split(['/', '\\']).any(|s| s == "..");
    if bad {
        return Err(AppError::BadRequest(format!("invalid file path: {p}")));
    }
    Ok(())
}

/// Stages `paths` (everything when None) and commits with `message`.
pub async fn commit(dir: &Path, message: &str, paths: Option<&[String]>, log: &JobLog) -> AppResult<()> {
    require_git()?;
    if message.trim().is_empty() {
        return Err(AppError::BadRequest("enter a commit message".into()));
    }
    let mut add = git_cmd();
    add.arg("-C").arg(dir).args(["add"]);
    match paths {
        None => {
            log.push("Staging all changes ...");
            add.arg("-A");
        }
        Some([]) => return Err(AppError::BadRequest("select at least one file to commit".into())),
        Some(list) => {
            for p in list {
                validate_rel_path(p)?;
            }
            log.push(format!("Staging {} file(s) ...", list.len()));
            add.arg("--").args(list);
        }
    }
    run_streaming(add, log, "git add").await?;
    log.push("Committing ...");
    let mut cmd = git_cmd();
    cmd.arg("-C").arg(dir).args(["commit", "-m", message.trim()]);
    if paths.is_some() {
        // Only commit what was selected, even if other files were staged before.
        cmd.arg("--only").arg("--").args(paths.unwrap());
    }
    run_streaming(cmd, log, "git commit").await
}

pub async fn push(dir: &Path, log: &JobLog) -> AppResult<()> {
    require_git()?;
    let st = status(dir).await?;
    let mut cmd = git_cmd();
    cmd.arg("-C").arg(dir).arg("push").arg("--progress");
    if st.upstream.is_none() {
        let branch = st.branch.ok_or_else(|| AppError::BadRequest("cannot push a detached HEAD".into()))?;
        validate_branch(&branch)?;
        log.push(format!("Running git push --set-upstream origin {branch} ..."));
        cmd.args(["--set-upstream", "origin", &branch]);
    } else {
        log.push("Running git push ...");
    }
    run_streaming(cmd, log, "git push").await
}

pub async fn fetch(dir: &Path, log: &JobLog) -> AppResult<()> {
    require_git()?;
    log.push("Running git fetch ...");
    let mut cmd = git_cmd();
    cmd.arg("-C").arg(dir).args(["fetch", "--progress"]);
    run_streaming(cmd, log, "git fetch").await
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
    let remote = out(dir, &["remote", "get-url", "origin"]).await.map(|u| redact(&u));
    let st = status(dir).await.unwrap_or_default();
    Some(GitInfo { branch: st.branch, remote, upstream: st.upstream, ahead: st.ahead, behind: st.behind, changed: st.files.len() as u32 })
}

static CACHE: Mutex<Option<HashMap<String, (Instant, Option<GitInfo>)>>> = Mutex::new(None);
static FETCHED: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

/// Quietly fetches in the background (at most every 5 minutes per repository) so "behind" stays current.
fn maybe_background_fetch(dir: &Path, info: &GitInfo) {
    if info.upstream.is_none() {
        return;
    }
    let key = dir.to_string_lossy().to_string();
    {
        let mut g = FETCHED.lock().unwrap();
        let m = g.get_or_insert_with(HashMap::new);
        if m.get(&key).is_some_and(|t| t.elapsed() < Duration::from_secs(300)) {
            return;
        }
        m.insert(key, Instant::now());
    }
    let dir = dir.to_path_buf();
    tokio::spawn(async move {
        let mut cmd = git_cmd();
        cmd.arg("-C").arg(&dir).args(["fetch", "--quiet"]).stdout(Stdio::null()).stderr(Stdio::null());
        let _ = tokio::time::timeout(Duration::from_secs(60), cmd.status()).await;
        forget(&dir);
    });
}

/// `info` with a 30 second cache, because the dashboard polls the project list every few seconds.
pub async fn info_cached(dir: &Path) -> Option<GitInfo> {
    let key = dir.to_string_lossy().to_string();
    if let Some((at, v)) = CACHE.lock().unwrap().as_ref().and_then(|m| m.get(&key)) {
        if at.elapsed() < Duration::from_secs(30) {
            return v.clone();
        }
    }
    let v = info(dir).await;
    if let Some(i) = &v {
        maybe_background_fetch(dir, i);
    }
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
    fn parses_status() {
        let raw = "## main...origin/main [ahead 2, behind 1]\0 M src/a.rs\0?? new file.txt\0R  b.rs\0a.rs\0";
        let st = parse_status(raw);
        assert_eq!((st.branch.as_deref(), st.upstream.as_deref(), st.ahead, st.behind), (Some("main"), Some("origin/main"), 2, 1));
        assert_eq!(st.files.len(), 3);
        assert_eq!(st.files[1].path, "new file.txt");
        assert_eq!(st.files[2].path, "b.rs");
        let st = parse_status("## No commits yet on dev\0");
        assert_eq!(st.branch.as_deref(), Some("dev"));
        assert!(st.upstream.is_none());
    }

    #[test]
    fn rel_paths() {
        assert!(validate_rel_path("src/a.rs").is_ok());
        assert!(validate_rel_path("../x").is_err());
        assert!(validate_rel_path("-rf").is_err());
        assert!(validate_rel_path("C:\\x").is_err());
    }

    #[test]
    fn names() {
        assert_eq!(name_from_url("https://github.com/WordPress/WordPress.git"), "wordpress");
        assert_eq!(name_from_url("git@github.com:me/My_Cool.Site.git"), "my-cool-site");
        assert_eq!(redact("https://user:tok@github.com/a/b.git"), "https://github.com/a/b.git");
    }
}
