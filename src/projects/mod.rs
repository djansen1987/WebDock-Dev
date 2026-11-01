//! Projects: persistence (projects.json), Nginx vhost generation and the create/delete/toggle workflows.

pub mod nginx;

use crate::cert;
use crate::config::{Config, PHP_VERSIONS};
use crate::docker::DockerClient;
use crate::error::{AppError, AppResult};
use crate::hosts::{self, parser::HostEntry};
use crate::jobs::JobLog;
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub domain: String,
    pub php_version: String,
    pub db_name: Option<String>,
    /// Sub-folder served by Nginx, e.g. "public" (Laravel/Symfony) or "" (WordPress).
    #[serde(default)]
    pub docroot: String,
    pub ssl: bool,
    pub enabled: bool,
    /// Absolute path of a folder outside the sites directory (existing project). Mounted at /ext/<name>.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_path: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// New empty project with a placeholder page.
    #[default]
    Empty,
    /// Clone a Git repository into the sites directory.
    Git,
    /// Use an existing folder anywhere on this PC.
    Folder,
}

#[derive(Debug, Deserialize)]
pub struct NewProject {
    pub name: String,
    pub domain: Option<String>,
    pub php_version: String,
    pub db_name: Option<String>,
    #[serde(default)]
    pub docroot: String,
    #[serde(default = "yes")]
    pub ssl: bool,
    #[serde(default)]
    pub source: Source,
    pub git_url: Option<String>,
    pub git_branch: Option<String>,
    pub folder: Option<String>,
    /// .sql file imported into `db_name` after creation (absolute, or relative to the project folder).
    pub sql_file: Option<String>,
}

fn yes() -> bool {
    true
}

pub fn load(cfg: &Config) -> Result<Vec<Project>> {
    match std::fs::read_to_string(cfg.projects_file()) {
        Ok(s) => Ok(serde_json::from_str(&s)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn save(cfg: &Config, projects: &[Project]) -> Result<()> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    let tmp = cfg.projects_file().with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(projects)?)?;
    std::fs::rename(tmp, cfg.projects_file())?;
    Ok(())
}

pub fn valid_ident(s: &str, extra: &[char]) -> bool {
    !s.is_empty() && s.len() <= 63 && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
}

fn validate(n: &NewProject) -> AppResult<()> {
    if !valid_ident(&n.name, &['-']) || n.name.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(AppError::BadRequest("name: use lowercase a-z, 0-9 and '-'".into()));
    }
    if !PHP_VERSIONS.contains(&n.php_version.as_str()) {
        return Err(AppError::BadRequest(format!("php_version must be one of {PHP_VERSIONS:?}")));
    }
    if let Some(db) = &n.db_name {
        if !db.is_empty() && !valid_ident(db, &['_']) {
            return Err(AppError::BadRequest("db_name: use letters, digits and '_'".into()));
        }
    }
    let d = &n.docroot;
    if d.contains("..") || d.starts_with('/') || !d.chars().all(|c| c.is_ascii_alphanumeric() || "-_/.".contains(c)) {
        return Err(AppError::BadRequest("docroot: invalid relative path".into()));
    }
    Ok(())
}

/// Checks a stored project (used when restoring a backup that may come from an untrusted file).
pub fn validate_project(p: &Project) -> AppResult<()> {
    validate(&NewProject {
        name: p.name.clone(),
        domain: Some(p.domain.clone()),
        php_version: p.php_version.clone(),
        db_name: p.db_name.clone(),
        docroot: p.docroot.clone(),
        ssl: p.ssl,
        source: Source::Empty,
        git_url: None,
        git_branch: None,
        folder: None,
        sql_file: None,
    })?;
    if let Some(path) = &p.custom_path {
        validate_folder_path(path)?;
    }
    hosts::parser::validate_domain(&p.domain).map_err(AppError::BadRequest)
}

fn validate_folder_path(path: &str) -> AppResult<()> {
    if path.chars().any(|c| c.is_control()) || !std::path::Path::new(path).is_absolute() {
        return Err(AppError::BadRequest("folder: enter the full path of a folder, for example C:\\Projects\\my-site".into()));
    }
    Ok(())
}

/// Host folder holding the project's files.
pub fn site_path(cfg: &Config, p: &Project) -> std::path::PathBuf {
    match &p.custom_path {
        Some(path) => std::path::PathBuf::from(path),
        None => cfg.sites_dir.join(&p.name),
    }
}

/// Path of the project folder inside the proxy and PHP containers.
pub fn container_dir(p: &Project) -> String {
    if p.custom_path.is_some() {
        format!("/ext/{}", p.name)
    } else {
        format!("/var/www/{}", p.name)
    }
}

/// Extra bind mounts (container path, host path) for projects living outside the sites directory.
pub fn custom_mounts(cfg: &Config) -> Vec<(String, String)> {
    load(cfg)
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.custom_path.as_ref().map(|path| (container_dir(p), path.clone())))
        .collect()
}

/// Host folder served by Nginx (project folder + docroot).
pub fn web_root(cfg: &Config, p: &Project) -> std::path::PathBuf {
    if p.docroot.is_empty() {
        site_path(cfg, p)
    } else {
        site_path(cfg, p).join(&p.docroot)
    }
}

/// Writes a default index.html unless the web root already contains an index file.
pub(crate) fn write_placeholder(cfg: &Config, p: &Project) -> Result<()> {
    // Never add files to a user's own folder or to a Git work tree.
    if p.custom_path.is_some() || site_path(cfg, p).join(".git").exists() {
        return Ok(());
    }
    let root = web_root(cfg, p);
    let has_index = std::fs::read_dir(&root)?
        .flatten()
        .any(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with("index."));
    if has_index {
        return Ok(());
    }
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let html = include_str!("placeholder.html")
        .replace("{{domain}}", &esc(&p.domain))
        .replace("{{name}}", &esc(&p.name))
        .replace("{{php}}", &esc(&p.php_version))
        .replace("{{path}}", &esc(&site_path(cfg, p).display().to_string()))
        .replace("{{webroot}}", &esc(&root.display().to_string()));
    std::fs::write(root.join("index.html"), html)?;
    Ok(())
}

/// Create workflow: validate -> infra -> (clone) -> hosts entry -> certificate -> database -> vhost -> reload proxy.
/// Any failure rolls back the hosts entry, certificate, vhost and a newly created site folder.
/// Existing folders (`Source::Folder`) are never modified or removed.
pub async fn create(cfg: &Config, docker: &DockerClient, n: NewProject, log: &JobLog) -> AppResult<Project> {
    validate(&n)?;
    let domain = n.domain.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| format!("{}.test", n.name));
    hosts::parser::validate_domain(&domain).map_err(AppError::BadRequest)?;
    let db_name = n.db_name.clone().filter(|d| !d.is_empty());
    let sql_file = n.sql_file.clone().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if sql_file.is_some() && db_name.is_none() {
        return Err(AppError::BadRequest("enter a database name to import a SQL file into".into()));
    }

    let mut clone_from = None;
    let mut custom_path = None;
    match n.source {
        Source::Empty => {}
        Source::Git => {
            crate::git::require_git()?;
            let url = crate::git::normalize_url(n.git_url.as_deref().unwrap_or(""))?;
            let branch = n.git_branch.as_deref().map(str::trim).filter(|b| !b.is_empty()).map(String::from);
            if let Some(b) = &branch {
                crate::git::validate_branch(b)?;
            }
            clone_from = Some((url, branch));
        }
        Source::Folder => {
            let raw = n.folder.as_deref().unwrap_or("").trim();
            let trimmed = raw.trim_end_matches(['\\', '/']);
            // Keep a drive root such as "C:\" intact.
            let path = if trimmed.ends_with(':') { raw.to_string() } else { trimmed.to_string() };
            validate_folder_path(&path)?;
            if !std::path::Path::new(&path).is_dir() {
                return Err(AppError::BadRequest(format!("folder does not exist: {path}")));
            }
            custom_path = Some(path);
        }
    }

    let projects = load(cfg)?;
    if projects.iter().any(|p| p.name == n.name || p.domain == domain) {
        return Err(AppError::Conflict("a project with this name or domain already exists".into()));
    }
    if let Some(path) = &custom_path {
        let same = |a: &str, b: &str| a.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(b.trim_end_matches(['\\', '/']));
        if projects.iter().any(|p| p.custom_path.as_deref().map_or(false, |c| same(c, path))) {
            return Err(AppError::Conflict("another project already uses this folder".into()));
        }
    }
    let mut project = Project {
        name: n.name,
        domain,
        php_version: n.php_version,
        db_name,
        docroot: n.docroot.trim_matches('/').to_string(),
        ssl: n.ssl,
        enabled: true,
        custom_path,
    };

    let site_dir = site_path(cfg, &project);
    let owns_dir = project.custom_path.is_none();
    let site_dir_existed = site_dir.exists();
    if clone_from.is_some() && site_dir_existed && std::fs::read_dir(&site_dir)?.next().is_some() {
        return Err(AppError::Conflict(format!("{} already exists and is not empty", site_dir.display())));
    }

    // Fail early (and provision the base services) before anything on the system is modified.
    // A project outside the sites directory is saved first: the containers read it to mount the folder.
    if project.custom_path.is_some() {
        let mut with = projects.clone();
        with.push(project.clone());
        save(cfg, &with)?;
    }
    let prepared = async {
        docker.ensure_infra(cfg, log).await?;
        docker.ensure_php(cfg, &project.php_version, log).await
    }
    .await;
    if let Err(e) = prepared {
        if project.custom_path.is_some() {
            let _ = save(cfg, &projects);
        }
        return Err(e);
    }

    if let Some((url, branch)) = &clone_from {
        if let Err(e) = crate::git::clone(url, branch.as_deref(), &site_dir, log).await {
            if !site_dir_existed {
                let _ = std::fs::remove_dir_all(&site_dir);
            }
            return Err(e);
        }
    } else if owns_dir {
        log.push(format!("Creating site folder {}", site_dir.display()));
        std::fs::create_dir_all(site_dir.join(&project.docroot))?;
        write_placeholder(cfg, &project)?;
    }
    if clone_from.is_some() || !owns_dir {
        if project.docroot.is_empty() && !has_index(&site_dir) && site_dir.join("public").join("index.php").is_file() {
            log.push("Found public/index.php: using \"public\" as the document root.");
            project.docroot = "public".into();
        }
    }

    log.push(format!("==> Adding hosts entry for {} (Windows may ask for administrator permission)", project.domain));
    let hosts_path = cfg.hosts_path.clone();
    let entry = HostEntry { ip: "127.0.0.1".into(), domain: project.domain.clone() };
    let added = {
        let hp = hosts_path.clone();
        tokio::task::spawn_blocking(move || hosts::add(&hp, entry)).await.map_err(anyhow::Error::from)?
    };
    if let Err(e) = added {
        if owns_dir && !site_dir_existed {
            let _ = std::fs::remove_dir_all(&site_dir);
        }
        if project.custom_path.is_some() {
            let _ = save(cfg, &projects);
        }
        return Err(e);
    }

    let result = finish_create(cfg, docker, &project, projects.clone(), sql_file.as_deref(), log).await;
    if result.is_err() {
        log.push("Rolling back changes...");
        let (certs, d) = (cfg.certs_dir(), project.domain.clone());
        let _ = tokio::task::spawn_blocking(move || {
            let _ = hosts::remove(&hosts_path, &d);
            let _ = cert::remove(&certs, &d);
        })
        .await;
        let _ = std::fs::remove_file(cfg.nginx_conf_dir().join(format!("{}.conf", project.name)));
        if owns_dir && !site_dir_existed {
            let _ = std::fs::remove_dir_all(&site_dir);
        }
        if project.custom_path.is_some() {
            let _ = save(cfg, &projects);
        }
    }
    result.map(|_| project)
}

fn has_index(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with("index.")))
        .unwrap_or(false)
}

/// Resolves a user-supplied SQL file: absolute, or relative to the project folder (no `..`).
fn resolve_sql(cfg: &Config, p: &Project, input: &str) -> AppResult<std::path::PathBuf> {
    let raw = std::path::Path::new(input.trim());
    let path = if raw.is_absolute() {
        raw.to_path_buf()
    } else if raw.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(AppError::BadRequest("sql file: '..' is not allowed in a relative path".into()));
    } else {
        site_path(cfg, p).join(raw)
    };
    let is_sql = path.extension().map_or(false, |e| e.eq_ignore_ascii_case("sql"));
    if !is_sql {
        return Err(AppError::BadRequest("sql file: only .sql files can be imported".into()));
    }
    if !path.is_file() {
        return Err(AppError::BadRequest(format!("sql file not found: {}", path.display())));
    }
    Ok(path)
}

/// `.sql` files in a project folder (relative paths, at most 4 levels deep), for the import picker.
pub fn find_sql_files(dir: &std::path::Path) -> Vec<String> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, depth: usize, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            if out.len() >= 50 {
                return;
            }
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if depth < 4 && !matches!(name.as_str(), ".git" | "node_modules" | "vendor" | "cache" | "target") {
                    walk(root, &path, depth + 1, out);
                }
            } else if path.extension().map_or(false, |x| x.eq_ignore_ascii_case("sql")) {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, 0, &mut out);
    out.sort();
    out
}

/// Creates (if needed) and links a database to a project, optionally importing a .sql file into it.
pub async fn link_database(cfg: &Config, docker: &DockerClient, name: &str, db: &str, sql_file: Option<&str>, log: &JobLog) -> AppResult<()> {
    if !valid_ident(db, &['_']) || crate::docker::SYSTEM_DATABASES.iter().any(|d| d.eq_ignore_ascii_case(db)) {
        return Err(AppError::BadRequest("database name: use letters, digits and '_' (not a system database)".into()));
    }
    let mut projects = load(cfg)?;
    let p = projects.iter_mut().find(|p| p.name == name).ok_or_else(|| AppError::NotFound("project not found".into()))?;
    let sql_path = match sql_file.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(resolve_sql(cfg, p, s)?),
        None => None,
    };
    p.db_name = Some(db.to_string());
    docker.ensure_infra(cfg, log).await?;
    log.push(format!("Creating database `{db}` if it does not exist..."));
    docker.create_database(db, log).await?;
    save(cfg, &projects)?;
    log.push(format!("Database `{db}` is linked to project \"{name}\"."));
    if let Some(path) = sql_path {
        docker.import_sql(db, &path, log).await?;
    }
    Ok(())
}

async fn finish_create(
    cfg: &Config,
    docker: &DockerClient,
    project: &Project,
    mut projects: Vec<Project>,
    sql_file: Option<&str>,
    log: &JobLog,
) -> AppResult<()> {
    if project.ssl {
        log.push("==> Generating SSL certificate");
        let (dir, d) = (cfg.certs_dir(), project.domain.clone());
        let generator = tokio::task::spawn_blocking(move || cert::generate(&dir, &d)).await.map_err(anyhow::Error::from)??;
        log.push(format!("Certificate created with {generator}."));
    }
    if let Some(db) = &project.db_name {
        log.push(format!("==> Creating database `{db}`"));
        docker.create_database(db, log).await?;
        if let Some(file) = sql_file {
            // A broken dump must not undo the whole project (a clone can take a while); report and continue.
            let imported = match resolve_sql(cfg, project, file) {
                Ok(path) => docker.import_sql(db, &path, log).await,
                Err(e) => Err(e),
            };
            if let Err(e) = imported {
                log.push(format!("Warning: SQL import failed: {e}"));
                log.push("The project was created. Fix the file and import it again with the database button of the project.");
            }
        }
    }
    apply_vhost(cfg, docker, project, log).await?;
    projects.push(project.clone());
    save(cfg, &projects)?;
    log.push("Project created.");
    Ok(())
}

/// Writes or removes the vhost depending on `enabled`, ensures the PHP container runs, reloads Nginx.
pub async fn apply_vhost(cfg: &Config, docker: &DockerClient, p: &Project, log: &JobLog) -> AppResult<()> {
    let conf_dir = cfg.nginx_conf_dir();
    std::fs::create_dir_all(&conf_dir)?;
    let file = conf_dir.join(format!("{}.conf", p.name));
    if p.enabled {
        log.push("==> Writing Nginx configuration");
        let have_cert = p.ssl && cert::exists(&cfg.certs_dir(), &p.domain);
        std::fs::write(&file, nginx::render_vhost(p, have_cert))?;
        docker.ensure_php(cfg, &p.php_version, log).await?;
    } else if file.exists() {
        log.push("Removing Nginx configuration...");
        std::fs::remove_file(&file)?;
    }
    log.push("Reloading proxy...");
    docker.reload_proxy().await?;
    Ok(())
}

pub async fn set_enabled(cfg: &Config, docker: &DockerClient, name: &str, enabled: bool, log: &JobLog) -> AppResult<Project> {
    let mut projects = load(cfg)?;
    let p = projects.iter_mut().find(|p| p.name == name).ok_or_else(|| AppError::NotFound("project not found".into()))?;
    p.enabled = enabled;
    let p = p.clone();
    docker.ensure_infra(cfg, log).await?;
    if enabled {
        if p.custom_path.is_none() {
            std::fs::create_dir_all(web_root(cfg, &p))?;
        }
        write_placeholder(cfg, &p)?;
    }
    save(cfg, &projects)?;
    apply_vhost(cfg, docker, &p, log).await?;
    Ok(p)
}

/// Switches a project to another PHP version: starts that container, rewrites the vhost and reloads the proxy.
pub async fn set_php_version(cfg: &Config, docker: &DockerClient, name: &str, version: &str, log: &JobLog) -> AppResult<Project> {
    if !PHP_VERSIONS.contains(&version) {
        return Err(AppError::BadRequest(format!("php_version must be one of {PHP_VERSIONS:?}")));
    }
    let mut projects = load(cfg)?;
    let p = projects.iter_mut().find(|p| p.name == name).ok_or_else(|| AppError::NotFound("project not found".into()))?;
    if p.php_version == version {
        log.push(format!("Project already uses PHP {version}."));
        return Ok(p.clone());
    }
    let previous = std::mem::replace(&mut p.php_version, version.to_string());
    let p = p.clone();
    log.push(format!("Switching \"{name}\" from PHP {previous} to PHP {version}..."));
    docker.ensure_infra(cfg, log).await?;
    docker.ensure_php(cfg, version, log).await?;
    save(cfg, &projects)?;
    apply_vhost(cfg, docker, &p, log).await?;
    log.push(format!("Project now runs on PHP {version}."));
    Ok(p)
}

pub async fn delete(cfg: &Config, docker: &DockerClient, name: &str, log: &JobLog) -> AppResult<()> {
    let mut projects = load(cfg)?;
    let idx = projects.iter().position(|p| p.name == name).ok_or_else(|| AppError::NotFound("project not found".into()))?;
    let p = projects.remove(idx);
    save(cfg, &projects)?;
    let mut gone = p.clone();
    gone.enabled = false;
    if let Err(e) = apply_vhost(cfg, docker, &gone, log).await {
        log.push(format!("Warning: could not update the proxy: {e}"));
    }
    log.push(format!("Removing hosts entry and certificate for {} (Windows may ask for administrator permission)...", p.domain));
    let (hp, d, certs) = (cfg.hosts_path.clone(), p.domain.clone(), cfg.certs_dir());
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        hosts::remove(&hp, &d)?;
        cert::remove(&certs, &d)?;
        Ok(())
    })
    .await
    .map_err(anyhow::Error::from)??;
    log.push("Project removed. Site files were kept on disk.");
    Ok(())
}
