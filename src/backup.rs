//! Config backup and restore.
//!
//! A backup is one JSON file in `<data>/backups` with the project list and the project certificates.
//! Nginx vhosts and hosts entries are derived from the projects, so a restore regenerates them.
//! Site files and databases are not part of a backup.

use crate::cert;
use crate::config::Config;
use crate::docker::DockerClient;
use crate::error::{AppError, AppResult};
use crate::hosts::{self, parser::HostEntry};
use crate::jobs::JobLog;
use crate::projects::{self, Project};
use crate::updates::{now_secs, ymd_from_days};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

const MAX_BACKUPS: usize = 10;
const MAX_IMPORT_BYTES: usize = 4 * 1024 * 1024;
/// A new backup is suggested when the newest one is older than this.
pub const DUE_AFTER_SECS: u64 = 30 * 24 * 3600;

#[derive(Debug, Serialize, Deserialize)]
struct Bundle {
    version: u32,
    created_at: u64,
    projects: Vec<Project>,
    /// Certificate files by file name (PEM text).
    certs: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct BackupInfo {
    pub name: String,
    pub created_at: u64,
    pub size: u64,
    pub projects: usize,
}

#[derive(Debug, Serialize)]
pub struct BackupList {
    pub backups: Vec<BackupInfo>,
    pub last_backup_at: Option<u64>,
    /// True when there is something to back up and the newest backup is missing or older than a month.
    pub due: bool,
    pub dir: PathBuf,
}

fn valid_name(name: &str) -> bool {
    name.starts_with("ldp-backup-")
        && name.ends_with(".json")
        && name.len() <= 80
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && !name.contains("..")
}

fn timestamp_name(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let date = ymd_from_days(days).replace('-', "");
    format!("ldp-backup-{date}-{:02}{:02}{:02}.json", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn parse(text: &str) -> AppResult<Bundle> {
    if text.len() > MAX_IMPORT_BYTES {
        return Err(AppError::BadRequest("backup file is too large".into()));
    }
    let b: Bundle = serde_json::from_str(text).map_err(|_| AppError::BadRequest("not a valid WebDock Dev backup file".into()))?;
    if b.version != 1 {
        return Err(AppError::BadRequest(format!("unsupported backup version {}", b.version)));
    }
    let mut seen = std::collections::HashSet::new();
    for p in &b.projects {
        projects::validate_project(p)?;
        if !seen.insert((p.name.clone(), p.domain.clone())) || b.projects.iter().filter(|o| o.name == p.name || o.domain == p.domain).count() > 1 {
            return Err(AppError::BadRequest(format!("duplicate project or domain: {}", p.name)));
        }
    }
    for (file, content) in &b.certs {
        let allowed = b.projects.iter().any(|p| *file == format!("{}.pem", p.domain) || *file == format!("{}-key.pem", p.domain));
        if !allowed || content.len() > 64 * 1024 {
            return Err(AppError::BadRequest(format!("unexpected certificate file in backup: {file}")));
        }
    }
    Ok(b)
}

pub fn list(cfg: &Config) -> BackupList {
    let mut backups = Vec::new();
    if let Ok(rd) = std::fs::read_dir(cfg.backups_dir()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !valid_name(&name) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
            let Ok(b) = serde_json::from_str::<Bundle>(&text) else { continue };
            backups.push(BackupInfo { name, created_at: b.created_at, size: text.len() as u64, projects: b.projects.len() });
        }
    }
    backups.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.name.cmp(&a.name)));
    let last_backup_at = backups.first().map(|b| b.created_at);
    let has_projects = projects::load(cfg).map(|p| !p.is_empty()).unwrap_or(false);
    let stale = last_backup_at.map_or(true, |t| now_secs().saturating_sub(t) > DUE_AFTER_SECS);
    BackupList { backups, last_backup_at, due: has_projects && stale, dir: cfg.backups_dir() }
}

fn write_bundle(cfg: &Config, bundle: &Bundle) -> AppResult<String> {
    let dir = cfg.backups_dir();
    std::fs::create_dir_all(&dir)?;
    let mut name = timestamp_name(bundle.created_at);
    let mut n = 1;
    while dir.join(&name).exists() {
        name = timestamp_name(bundle.created_at).replace(".json", &format!("-{n}.json"));
        n += 1;
    }
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(bundle).map_err(anyhow::Error::from)?)?;
    std::fs::rename(tmp, dir.join(&name))?;
    prune(cfg);
    Ok(name)
}

fn prune(cfg: &Config) {
    let all = list(cfg).backups;
    for old in all.iter().skip(MAX_BACKUPS) {
        let _ = std::fs::remove_file(cfg.backups_dir().join(&old.name));
    }
}

fn snapshot(cfg: &Config) -> AppResult<Bundle> {
    let projects = projects::load(cfg)?;
    let mut certs = BTreeMap::new();
    for p in &projects {
        let (c, k) = cert::cert_paths(&cfg.certs_dir(), &p.domain);
        for path in [c, k] {
            if let (Some(name), Ok(text)) = (path.file_name().map(|n| n.to_string_lossy().to_string()), std::fs::read_to_string(&path)) {
                certs.insert(name, text);
            }
        }
    }
    Ok(Bundle { version: 1, created_at: now_secs(), projects, certs })
}

pub fn create(cfg: &Config, log: &JobLog) -> AppResult<String> {
    let bundle = snapshot(cfg)?;
    log.push(format!("Backing up {} project(s) and {} certificate file(s)...", bundle.projects.len(), bundle.certs.len()));
    let name = write_bundle(cfg, &bundle)?;
    log.push(format!("Backup saved as {name} in {}", cfg.backups_dir().display()));
    Ok(name)
}

pub fn read_raw(cfg: &Config, name: &str) -> AppResult<String> {
    if !valid_name(name) {
        return Err(AppError::BadRequest("invalid backup name".into()));
    }
    std::fs::read_to_string(cfg.backups_dir().join(name)).map_err(|_| AppError::NotFound("backup not found".into()))
}

pub fn delete(cfg: &Config, name: &str, log: &JobLog) -> AppResult<()> {
    read_raw(cfg, name)?;
    std::fs::remove_file(cfg.backups_dir().join(name))?;
    log.push(format!("Deleted backup {name}."));
    Ok(())
}

/// Validates an uploaded backup and stores it next to the others.
pub fn import(cfg: &Config, content: &str, log: &JobLog) -> AppResult<String> {
    let mut bundle = parse(content)?;
    // Keep the original creation time so the monthly reminder reflects the data, not the upload.
    bundle.created_at = bundle.created_at.min(now_secs());
    let name = write_bundle(cfg, &bundle)?;
    log.push(format!("Imported backup as {name} ({} project(s)).", bundle.projects.len()));
    Ok(name)
}

/// Replaces the current configuration with the backup: projects, certificates, hosts entries and vhosts.
pub async fn restore(cfg: &Config, docker: &DockerClient, name: &str, log: &JobLog) -> AppResult<()> {
    let bundle = parse(&read_raw(cfg, name)?)?;
    docker.ensure_docker_ready(log).await?;

    log.push("Saving the current configuration first (safety backup)...");
    create(cfg, log)?;

    let current = projects::load(cfg)?;
    log.push(format!("Restoring {} project(s)...", bundle.projects.len()));
    std::fs::create_dir_all(cfg.certs_dir())?;
    for (file, content) in &bundle.certs {
        std::fs::write(cfg.certs_dir().join(file), content)?;
    }
    projects::save(cfg, &bundle.projects)?;

    log.push("Updating the hosts file (Windows may ask for administrator permission)...");
    let remove: Vec<String> = current.iter().map(|p| p.domain.clone()).collect();
    let add: Vec<HostEntry> = bundle.projects.iter().map(|p| HostEntry { ip: "127.0.0.1".into(), domain: p.domain.clone() }).collect();
    let hosts_path = cfg.hosts_path.clone();
    tokio::task::spawn_blocking(move || hosts::sync(&hosts_path, &remove, &add)).await.map_err(anyhow::Error::from)??;

    docker.ensure_infra(cfg, log).await?;

    for old in current.iter().filter(|c| !bundle.projects.iter().any(|p| p.name == c.name)) {
        let mut gone = old.clone();
        gone.enabled = false;
        projects::apply_vhost(cfg, docker, &gone, log).await?;
    }
    for p in &bundle.projects {
        if p.ssl && !cert::exists(&cfg.certs_dir(), &p.domain) {
            log.push(format!("Certificate for {} is missing, generating a new one...", p.domain));
            let (dir, d) = (cfg.certs_dir(), p.domain.clone());
            tokio::task::spawn_blocking(move || cert::generate(&dir, &d)).await.map_err(anyhow::Error::from)??;
        }
        if p.enabled {
            if p.custom_path.is_none() {
                std::fs::create_dir_all(projects::web_root(cfg, p))?;
            }
            projects::write_placeholder(cfg, p)?;
        }
        projects::apply_vhost(cfg, docker, p, log).await?;
    }
    log.push("Configuration restored. Site files and databases were not touched.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("ldp-backup-20261101-190000.json"));
        assert!(!valid_name("../ldp-backup-1.json"));
        assert!(!valid_name("ldp-backup-1/2.json"));
        assert!(!valid_name("other.json"));
        assert_eq!(timestamp_name(1_793_559_600), "ldp-backup-20261101-190000.json");
    }

    #[test]
    fn rejects_bad_bundles() {
        assert!(parse("{}").is_err());
        let evil = r#"{"version":1,"created_at":1,"projects":[],"certs":{"../x.pem":"a"}}"#;
        assert!(parse(evil).is_err());
        let bad_project = r#"{"version":1,"created_at":1,"projects":[{"name":"A b","domain":"x.test","php_version":"8.3","db_name":null,"docroot":"","ssl":true,"enabled":true}],"certs":{}}"#;
        assert!(parse(bad_project).is_err());
        let ok = r#"{"version":1,"created_at":1,"projects":[{"name":"site","domain":"site.test","php_version":"8.3","db_name":null,"docroot":"","ssl":true,"enabled":true}],"certs":{"site.test.pem":"x"}}"#;
        assert!(parse(ok).is_ok());
    }
}
