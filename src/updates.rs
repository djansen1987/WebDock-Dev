//! Software freshness: newer-image checks, end-of-life checks (endoflife.date) and the update job.
//!
//! Component ids equal the docker compose service names (`proxy`, `mariadb`, `phpmyadmin`, `php83`, ...).

use crate::config::{php_container, Config, DB_CONTAINER, PHP_VERSIONS, PMA_CONTAINER, PROXY_CONTAINER};
use crate::docker::{self, DockerClient};
use crate::error::{AppError, AppResult};
use crate::jobs::JobLog;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CHECK_INTERVAL_SECS: u64 = 24 * 3600;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Supported,
    SecurityOnly,
    Eol,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    pub name: String,
    pub container: String,
    pub image: String,
    pub installed: Option<String>,
    pub latest: Option<String>,
    pub support: Support,
    pub eol_date: Option<String>,
    /// Some(true) = a newer image exists; None = could not be determined.
    pub image_update: Option<bool>,
    /// Projects using this PHP version (empty for non-PHP components).
    #[serde(default)]
    pub used_by: Vec<String>,
    #[serde(default)]
    pub removable: bool,
}

impl Component {
    pub fn needs_attention(&self) -> bool {
        self.support == Support::Eol || self.image_update == Some(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateReport {
    pub checked_at: u64,
    pub components: Vec<Component>,
    pub attention: usize,
    /// False when endoflife.date or the registry could not be reached.
    pub online: bool,
}

struct Spec {
    id: String,
    name: String,
    container: String,
    product: &'static str,
    env_key: &'static str,
    /// Image to compare against the registry; None = use the image the container was created from.
    base_image: Option<String>,
}

fn specs() -> Vec<Spec> {
    let mut v = vec![
        Spec { id: "proxy".into(), name: "Nginx proxy".into(), container: PROXY_CONTAINER.into(), product: "nginx", env_key: "NGINX_VERSION", base_image: None },
        Spec { id: "mariadb".into(), name: "MariaDB".into(), container: DB_CONTAINER.into(), product: "mariadb", env_key: "MARIADB_VERSION", base_image: None },
        Spec { id: "phpmyadmin".into(), name: "phpMyAdmin".into(), container: PMA_CONTAINER.into(), product: "phpmyadmin", env_key: "VERSION", base_image: None },
    ];
    for ver in PHP_VERSIONS {
        v.push(Spec {
            id: format!("php{}", ver.replace('.', "")),
            name: format!("PHP {ver}"),
            container: php_container(ver),
            product: "php",
            env_key: "PHP_VERSION",
            base_image: Some(format!("php:{ver}-fpm-alpine")),
        });
    }
    v
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Converts days since 1970-01-01 to "YYYY-MM-DD" (proleptic Gregorian).
pub(crate) fn ymd_from_days(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn today() -> String {
    ymd_from_days((now_secs() / 86_400) as i64)
}

/// Support state from endoflife.date style dates ("YYYY-MM-DD" strings compare chronologically).
fn support_status(today: &str, support: Option<&str>, eol: Option<&str>) -> Support {
    if eol.is_some_and(|d| today >= d) {
        Support::Eol
    } else if support.is_some_and(|d| today >= d) {
        Support::SecurityOnly
    } else {
        Support::Supported
    }
}

/// "1:11.4.5+maria~ubu2404" -> "11.4.5"
fn clean_version(raw: &str) -> String {
    let s = raw.split_once(':').map_or(raw, |(_, r)| r);
    s.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<String>().trim_end_matches('.').to_string()
}

/// "8.3.36" -> "8.3"
fn cycle_of(version: &str) -> String {
    version.split('.').take(2).collect::<Vec<_>>().join(".")
}

fn version_tuple(v: &str) -> Vec<u64> {
    v.split('.').filter_map(|p| p.parse().ok()).collect()
}

fn is_newer(latest: &str, installed: &str) -> bool {
    version_tuple(latest) > version_tuple(installed)
}

/// endoflife.date uses a date string, `true` (already EOL) or `false` (no date yet).
fn parse_date(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Bool(true) => Some("0000-01-01".into()),
        _ => None,
    }
}

/// Offline fallback for PHP (support = active support ends, eol = security fixes end).
fn php_fallback() -> Vec<Value> {
    vec![
        json!({"cycle": "8.5", "support": "2027-12-31", "eol": "2029-12-31"}),
        json!({"cycle": "8.1", "support": "2023-11-25", "eol": "2025-12-31"}),
        json!({"cycle": "8.2", "support": "2024-12-31", "eol": "2026-12-31"}),
        json!({"cycle": "8.3", "support": "2025-12-31", "eol": "2027-12-31"}),
        json!({"cycle": "8.4", "support": "2026-12-31", "eol": "2028-12-31"}),
    ]
}

/// Support state per PHP version from the built-in table (used to flag old versions in the project wizard).
pub fn php_support() -> HashMap<String, Support> {
    let today = today();
    php_fallback()
        .iter()
        .filter_map(|e| {
            let cycle = e["cycle"].as_str()?.to_string();
            let st = support_status(&today, parse_date(e.get("support")).as_deref(), parse_date(e.get("eol")).as_deref());
            Some((cycle, st))
        })
        .collect()
}

async fn fetch_eol(product: &'static str) -> Option<Vec<Value>> {
    tokio::task::spawn_blocking(move || {
        ureq::get(&format!("https://endoflife.date/api/{product}.json"))
            .timeout(Duration::from_secs(10))
            .call()
            .ok()?
            .into_json::<Vec<Value>>()
            .ok()
    })
    .await
    .ok()
    .flatten()
}

async fn remote_digest(image: &str) -> Option<String> {
    let out = docker::docker_cli(&["buildx", "imagetools", "inspect", image, "--format", "{{.Manifest.Digest}}"], 40).await?;
    out.starts_with("sha256:").then_some(out)
}

fn env_value(env: &[String], key: &str) -> Option<String> {
    env.iter().find_map(|e| e.strip_prefix(key)?.strip_prefix('=')).map(str::to_string)
}

/// Checks every existing component. Progress is written to `log` when given (manual check job).
pub async fn check(cfg: &Config, docker: &DockerClient, log: Option<&JobLog>) -> Result<UpdateReport> {
    let say = |m: String| {
        if let Some(l) = log {
            l.push(m);
        }
    };
    let today = today();
    let mut online = true;
    let mut eol_cache: HashMap<&'static str, Option<Vec<Value>>> = HashMap::new();
    let mut components = Vec::new();
    let projects = crate::projects::load(cfg).unwrap_or_default();

    for s in specs() {
        let Some((container_image, env)) = docker.container_info(&s.container).await else { continue };
        say(format!("Checking {}...", s.name));
        let image = s.base_image.clone().unwrap_or(container_image);
        let installed = env_value(&env, s.env_key).map(|v| clean_version(&v)).filter(|v| !v.is_empty());

        if !eol_cache.contains_key(s.product) {
            let fetched = fetch_eol(s.product).await;
            if fetched.is_none() {
                online = false;
            }
            eol_cache.insert(s.product, fetched);
        }
        let entries = eol_cache[s.product].clone().or_else(|| (s.product == "php").then(php_fallback));
        let entry = installed.as_deref().and_then(|v| {
            let c = cycle_of(v);
            entries.as_ref()?.iter().find(|e| e["cycle"].as_str() == Some(c.as_str())).cloned()
        });
        let (support, eol_date, latest) = match &entry {
            Some(e) => {
                let (sup, eol) = (parse_date(e.get("support")), parse_date(e.get("eol")));
                let mut st = support_status(&today, sup.as_deref(), eol.as_deref());
                // The newest release line without an EOL date is the current one, even if upstream flags active support as ended.
                let newest = entries.as_ref().and_then(|l| l.first()).is_some_and(|f| f["cycle"] == e["cycle"]);
                if st == Support::SecurityOnly && eol.is_none() && newest {
                    st = Support::Supported;
                }
                (
                    st,
                    eol.filter(|d| !d.starts_with("0000")),
                    e["latest"].as_str().map(str::to_string),
                )
            }
            None => (Support::Unknown, None, None),
        };

        let local = docker.image_digests(&image).await;
        let by_version = || match (&latest, &installed) {
            (Some(l), Some(i)) => Some(is_newer(l, i)),
            _ => None,
        };
        let image_update = match remote_digest(&image).await {
            Some(remote) if !local.is_empty() => Some(!local.iter().any(|d| d.split('@').nth(1) == Some(remote.as_str()))),
            Some(_) => by_version(),
            None => {
                online = false;
                by_version()
            }
        };

        let is_php = s.product == "php";
        let used_by: Vec<String> = if is_php {
            let ver = s.name.trim_start_matches("PHP ");
            projects.iter().filter(|p| p.php_version == ver).map(|p| p.name.clone()).collect()
        } else {
            vec![]
        };
        let removable = is_php && used_by.is_empty();
        components.push(Component { id: s.id, name: s.name, container: s.container, image, installed, latest, support, eol_date, image_update, used_by, removable });
    }

    let attention = components.iter().filter(|c| c.needs_attention()).count();
    let report = UpdateReport { checked_at: now_secs(), components, attention, online };
    if let Ok(text) = serde_json::to_string_pretty(&report) {
        let _ = std::fs::write(cfg.updates_file(), text);
    }
    say(format!("Check finished: {} item(s) need attention.", report.attention));
    Ok(report)
}

pub fn load_cached(cfg: &Config) -> Option<UpdateReport> {
    serde_json::from_str(&std::fs::read_to_string(cfg.updates_file()).ok()?).ok()
}

/// Background task: refreshes the cached report once a day while Docker is running.
pub async fn scheduler(cfg: Arc<Config>, docker: DockerClient) {
    tokio::time::sleep(Duration::from_secs(20)).await;
    loop {
        let stale = load_cached(&cfg).map_or(true, |r| now_secs().saturating_sub(r.checked_at) > CHECK_INTERVAL_SECS);
        if stale && docker.ping().await {
            match check(&cfg, &docker, None).await {
                Ok(r) => tracing::info!("update check: {} item(s) need attention", r.attention),
                Err(e) => tracing::warn!("update check failed: {e}"),
            }
        }
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

/// Pulls newer images, rebuilds PHP images on a fresh base image, recreates the containers and reloads the proxy.
/// `ids` empty = all existing components. Data volumes and site files are untouched.
pub async fn apply(cfg: &Config, docker: &DockerClient, ids: &[String], log: &JobLog) -> AppResult<()> {
    docker.ensure_docker_ready(log).await?;
    // "Update all" only touches components the last check flagged, to stay within registry rate limits.
    let outdated: Option<Vec<String>> = if ids.is_empty() {
        load_cached(cfg).map(|r| r.components.into_iter().filter(|c| c.image_update != Some(false)).map(|c| c.id).collect())
    } else {
        None
    };
    let mut selected = Vec::new();
    for s in specs() {
        if !ids.is_empty() && !ids.contains(&s.id) {
            continue;
        }
        if outdated.as_ref().is_some_and(|o| !o.contains(&s.id)) {
            continue;
        }
        if docker.container_info(&s.container).await.is_some() {
            selected.push(s.id);
        }
    }
    if let Some(bad) = ids.iter().find(|i| !specs().iter().any(|s| &s.id == *i)) {
        return Err(AppError::BadRequest(format!("unknown component '{bad}'")));
    }
    if selected.is_empty() {
        log.push("Nothing to update: everything is up to date or no containers exist yet.");
        return Ok(());
    }

    let (php, other): (Vec<&String>, Vec<&String>) = selected.iter().partition(|i| i.starts_with("php") && i.as_str() != "phpmyadmin");
    if !other.is_empty() {
        log.push("Downloading the newest images...");
        let mut args = vec!["pull"];
        args.extend(other.iter().map(|s| s.as_str()));
        docker::compose(cfg, &args, log).await?;
    }
    if !php.is_empty() {
        log.push("Rebuilding PHP images on the newest base image...");
        let mut args = vec!["build", "--pull"];
        args.extend(php.iter().map(|s| s.as_str()));
        docker::compose(cfg, &args, log).await?;
    }
    log.push("Recreating containers...");
    let mut args = vec!["up", "-d"];
    args.extend(selected.iter().map(|s| s.as_str()));
    docker::compose(cfg, &args, log).await?;

    if docker.status(PROXY_CONTAINER).await.running {
        log.push("Reloading proxy...");
        docker.reload_proxy().await?;
    }
    log.push("Removing unused images...");
    let _ = docker::docker_cli(&["image", "prune", "-f"], 120).await;

    log.push("Re-checking versions...");
    check(cfg, docker, Some(log)).await?;
    Ok(())
}

/// Removes the container and image of a PHP version that no project uses. It is recreated on demand when needed again.
pub async fn remove_php(cfg: &Config, docker: &DockerClient, version: &str, log: &JobLog) -> AppResult<()> {
    if !PHP_VERSIONS.contains(&version) {
        return Err(AppError::BadRequest(format!("php version must be one of {PHP_VERSIONS:?}")));
    }
    let users: Vec<String> = crate::projects::load(cfg)?.into_iter().filter(|p| p.php_version == version).map(|p| p.name).collect();
    if !users.is_empty() {
        return Err(AppError::Conflict(format!("PHP {version} is still used by: {}", users.join(", "))));
    }
    docker.ensure_docker_ready(log).await?;
    log.push(format!("Removing the PHP {version} container..."));
    let name = php_container(version);
    if docker.container_info(&name).await.is_some() {
        docker::docker_cli(&["rm", "-f", &name], 60).await.ok_or_else(|| AppError::Internal(anyhow::anyhow!("could not remove container {name}")))?;
    }
    let _ = docker::docker_cli(&["rmi", &format!("ldp-php:{version}")], 60).await;
    check(cfg, docker, Some(log)).await?;
    Ok(())
}

/// Best-effort refresh of the cached report after the set of used PHP versions changed.
pub async fn refresh_quietly(cfg: &Config, docker: &DockerClient) {
    if let Err(e) = check(cfg, docker, None).await {
        tracing::warn!("update check failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(ymd_from_days(0), "1970-01-01");
        assert_eq!(ymd_from_days(20_088), "2024-12-31");
        assert_eq!(ymd_from_days(20_454), "2026-01-01");
    }

    #[test]
    fn support_states() {
        assert_eq!(support_status("2026-11-01", Some("2025-12-31"), Some("2027-12-31")), Support::SecurityOnly);
        assert_eq!(support_status("2026-11-01", Some("2023-11-25"), Some("2025-12-31")), Support::Eol);
        assert_eq!(support_status("2026-11-01", Some("2026-12-31"), Some("2028-12-31")), Support::Supported);
        assert_eq!(support_status("2026-11-01", None, None), Support::Supported);
    }

    #[test]
    fn versions() {
        assert_eq!(clean_version("1:11.4.5+maria~ubu2404"), "11.4.5");
        assert_eq!(clean_version("8.3.36"), "8.3.36");
        assert_eq!(cycle_of("1.28.0"), "1.28");
        assert!(is_newer("8.3.10", "8.3.9"));
        assert!(!is_newer("8.3.9", "8.3.9"));
    }

    #[test]
    fn eol_dates() {
        assert_eq!(parse_date(Some(&json!("2025-01-01"))).as_deref(), Some("2025-01-01"));
        assert_eq!(parse_date(Some(&json!(false))), None);
        assert!(parse_date(Some(&json!(true))).is_some());
    }

    #[test]
    fn env_lookup() {
        let env = vec!["A=1".to_string(), "PHP_VERSION=8.3.36".to_string()];
        assert_eq!(env_value(&env, "PHP_VERSION").as_deref(), Some("8.3.36"));
        assert_eq!(env_value(&env, "PHP"), None);
    }
}
