//! Runtime configuration. All paths can be overridden with environment variables.

use std::path::PathBuf;

pub const MARKER_BEGIN: &str = "# BEGIN LOCAL DEV PORTAL";
pub const MARKER_END: &str = "# END LOCAL DEV PORTAL";
/// File in the data dir holding the projects folder chosen in the installer.
pub const SITES_DIR_FILE: &str = "sites_dir.txt";
pub const PHP_VERSIONS: [&str; 5] = ["8.1", "8.2", "8.3", "8.4", "8.5"];
pub const PROXY_CONTAINER: &str = "ldp-proxy";
pub const DB_CONTAINER: &str = "ldp-mariadb";
pub const PMA_CONTAINER: &str = "ldp-phpmyadmin";

/// Name of the PHP-FPM container for a PHP version, e.g. "8.3" -> "ldp-php83".
pub fn php_container(version: &str) -> String {
    format!("ldp-php{}", version.replace('.', ""))
}

/// All containers the portal is allowed to manage.
pub fn managed_containers() -> Vec<String> {
    let mut v = vec![PROXY_CONTAINER.to_string(), DB_CONTAINER.to_string(), PMA_CONTAINER.to_string()];
    v.extend(PHP_VERSIONS.iter().map(|p| php_container(p)));
    v
}

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    /// Holds projects.json, certs/ and nginx/conf.d/ (mounted into the containers).
    pub data_dir: PathBuf,
    /// Host directory containing one folder per project (mounted at /var/www).
    pub sites_dir: PathBuf,
    pub hosts_path: PathBuf,
}

/// Loads `KEY=value` lines from `<data>\.env` into the process environment (real environment variables win).
/// Compose child processes inherit these, so `LDP_DB_ROOT_PASSWORD` set here reaches MariaDB and phpMyAdmin.
fn load_env_file(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim().trim_matches(|c| c == '"' || c == '\''));
            if k.starts_with("LDP_") && std::env::var_os(k).is_none() {
                std::env::set_var(k, v);
            }
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
        let data_dir = env("LDP_DATA_DIR").unwrap_or_else(default_data_dir);
        let _ = std::fs::create_dir_all(&data_dir);
        let data_dir = std::fs::canonicalize(&data_dir).unwrap_or(data_dir);
        // Docker does not understand the Windows verbatim prefix.
        let data_dir = PathBuf::from(data_dir.to_string_lossy().trim_start_matches(r"\\?\"));
        load_env_file(&data_dir.join(".env"));
        // Precedence: LDP_SITES_DIR, then <data>/sites_dir.txt (written by the installer), then <data>/sites.
        let from_file = std::fs::read_to_string(data_dir.join(SITES_DIR_FILE))
            .ok()
            .map(|s| s.trim().trim_start_matches('\u{feff}').trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        Self {
            port: std::env::var("LDP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(9000),
            sites_dir: env("LDP_SITES_DIR").or(from_file).unwrap_or_else(|| data_dir.join("sites")),
            hosts_path: env("LDP_HOSTS_FILE").unwrap_or_else(default_hosts_path),
            data_dir,
        }
    }
    pub fn certs_dir(&self) -> PathBuf {
        self.data_dir.join("certs")
    }
    pub fn nginx_conf_dir(&self) -> PathBuf {
        self.data_dir.join("nginx").join("conf.d")
    }
    pub fn projects_file(&self) -> PathBuf {
        self.data_dir.join("projects.json")
    }
    /// Cached result of the last update check.
    pub fn updates_file(&self) -> PathBuf {
        self.data_dir.join("updates.json")
    }
    /// Config backups (projects + certificates) as JSON bundles.
    pub fn backups_dir(&self) -> PathBuf {
        self.data_dir.join("backups")
    }
}

/// Per-user data folder: %LOCALAPPDATA%\WebDockDev on Windows, ~/.webdock-dev elsewhere.
pub fn default_data_dir() -> PathBuf {
    if cfg!(windows) {
        if let Some(base) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(base).join("WebDockDev");
        }
    } else if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".webdock-dev");
    }
    PathBuf::from("data")
}

pub fn default_hosts_path() -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        PathBuf::from(root).join(r"System32\drivers\etc\hosts")
    } else {
        PathBuf::from("/etc/hosts")
    }
}
