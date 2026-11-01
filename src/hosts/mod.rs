//! Safe hosts-file management: marker block, backup before every write, atomic replace,
//! and automatic privilege escalation through a minimal helper.

pub mod parser;
pub mod privilege;

use crate::error::{AppError, AppResult};
use anyhow::{Context, Result};
use parser::{outside_block, parse_block, render, validate_domain, validate_ip, HostEntry};
use std::path::{Path, PathBuf};

pub fn list(hosts: &Path) -> Result<Vec<HostEntry>> {
    let content = std::fs::read_to_string(hosts).with_context(|| format!("reading {}", hosts.display()))?;
    Ok(parse_block(&content))
}

fn check_conflicts(hosts: &Path, entries: &[HostEntry]) -> AppResult<()> {
    let content = std::fs::read_to_string(hosts).map_err(anyhow::Error::from)?;
    for e in entries {
        if let Some(line) = parser::external_entry(&content, &e.domain) {
            return Err(AppError::BadRequest(format!(
                "{} is already defined in your hosts file outside WebDock Dev (\"{line}\"). \
                 Remove or comment out that line (for example from another dev setup) or choose a different domain.",
                e.domain
            )));
        }
    }
    Ok(())
}

pub fn add(hosts: &Path, entry: HostEntry) -> AppResult<()> {
    validate_domain(&entry.domain).map_err(AppError::BadRequest)?;
    validate_ip(&entry.ip).map_err(AppError::BadRequest)?;
    check_conflicts(hosts, std::slice::from_ref(&entry))?;
    let mut entries = list(hosts)?;
    entries.retain(|e| e.domain != entry.domain);
    entries.push(entry);
    apply(hosts, &entries)?;
    Ok(())
}

/// Removes and adds several entries with a single write (one elevation prompt at most).
pub fn sync(hosts: &Path, remove_domains: &[String], add_entries: &[HostEntry]) -> AppResult<()> {
    for e in add_entries {
        validate_domain(&e.domain).map_err(AppError::BadRequest)?;
        validate_ip(&e.ip).map_err(AppError::BadRequest)?;
    }
    check_conflicts(hosts, add_entries)?;
    let mut entries = list(hosts)?;
    entries.retain(|e| !remove_domains.contains(&e.domain) && !add_entries.iter().any(|a| a.domain == e.domain));
    entries.extend(add_entries.iter().map(|e| HostEntry { ip: e.ip.clone(), domain: e.domain.clone() }));
    apply(hosts, &entries)?;
    Ok(())
}

pub fn remove(hosts: &Path, domain: &str) -> AppResult<()> {
    let mut entries = list(hosts)?;
    let before = entries.len();
    entries.retain(|e| e.domain != domain);
    if entries.len() == before {
        return Ok(());
    }
    apply(hosts, &entries)?;
    Ok(())
}

/// Writes the new managed block, escalating privileges only when direct access is denied.
fn apply(hosts: &Path, entries: &[HostEntry]) -> Result<()> {
    let current = std::fs::read_to_string(hosts)?;
    let new_content = render(&current, entries);
    if new_content == current {
        return Ok(());
    }
    match write_with_backup(hosts, &new_content) {
        Ok(()) => Ok(()),
        Err(e) if is_permission_denied(&e) => {
            let staged = tempfile::Builder::new().prefix("ldp-hosts-").tempfile()?;
            std::fs::write(staged.path(), &new_content)?;
            privilege::run_elevated_helper(staged.path(), hosts)
        }
        Err(e) => Err(e),
    }
}

fn is_permission_denied(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|c| c.downcast_ref::<std::io::Error>())
        .any(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
}

pub fn backup_path(hosts: &Path) -> PathBuf {
    let mut s = hosts.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

/// Backup (`<hosts>.bak`) then atomic replace via temp file in the same directory.
pub fn write_with_backup(hosts: &Path, content: &str) -> Result<()> {
    std::fs::copy(hosts, backup_path(hosts)).context("creating hosts backup")?;
    let mut tmp = hosts.as_os_str().to_owned();
    tmp.push(".ldp-tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, content)?;
    if let Ok(meta) = std::fs::metadata(hosts) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    if let Err(e) = std::fs::rename(&tmp, hosts) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

pub fn restore_backup(hosts: &Path) -> AppResult<()> {
    let bak = backup_path(hosts);
    let content = std::fs::read_to_string(&bak).map_err(|_| AppError::NotFound("no backup available".into()))?;
    match write_with_backup(hosts, &content) {
        Ok(()) => Ok(()),
        Err(e) if is_permission_denied(&e) => {
            let staged = tempfile::Builder::new().prefix("ldp-hosts-").tempfile()?;
            std::fs::write(staged.path(), &content)?;
            privilege::run_elevated_helper(staged.path(), hosts)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Entry point of the elevated helper: `--hosts-helper <staged> <target>`.
/// Only accepts changes confined to the managed block, so a compromised caller
/// cannot use the helper to rewrite arbitrary parts of the hosts file.
pub fn run_helper(staged: &Path, target: &Path) -> Result<()> {
    let new = std::fs::read_to_string(staged).context("reading staged file")?;
    let current = std::fs::read_to_string(target).context("reading target")?;
    if outside_block(&new) != outside_block(&current) {
        anyhow::bail!("refusing: staged file changes content outside the managed block");
    }
    for e in parse_block(&new) {
        validate_domain(&e.domain).map_err(anyhow::Error::msg)?;
        validate_ip(&e.ip).map_err(anyhow::Error::msg)?;
    }
    write_with_backup(target, &new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_roundtrip_with_backup() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = dir.path().join("hosts");
        std::fs::write(&hosts, "127.0.0.1 localhost\n").unwrap();
        add(&hosts, HostEntry { ip: "127.0.0.1".into(), domain: "a.test".into() }).unwrap();
        assert_eq!(list(&hosts).unwrap().len(), 1);
        assert_eq!(std::fs::read_to_string(backup_path(&hosts)).unwrap(), "127.0.0.1 localhost\n");
        remove(&hosts, "a.test").unwrap();
        assert_eq!(std::fs::read_to_string(&hosts).unwrap(), "127.0.0.1 localhost\n");
    }

    #[test]
    fn helper_rejects_changes_outside_block() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = dir.path().join("hosts");
        let staged = dir.path().join("staged");
        std::fs::write(&hosts, "127.0.0.1 localhost\n").unwrap();
        std::fs::write(&staged, "1.2.3.4 evil.com\n").unwrap();
        assert!(run_helper(&staged, &hosts).is_err());
    }
}
