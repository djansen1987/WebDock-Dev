//! HTTP layer: router, handlers, embedded Web UI and a localhost-only guard.

use crate::config::{Config, PHP_VERSIONS};
use crate::docker::{self, Action, DockerClient};
use crate::error::{AppError, AppResult};
use crate::jobs::{Job, JobLog, Jobs};
use crate::{cert, hosts, projects};
use axum::{
    extract::{Path, Query, Request, State},
    http::{header, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(RustEmbed)]
#[folder = "web/"]
struct Assets;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub docker: DockerClient,
    /// Serialises read-modify-write cycles on projects.json / hosts / vhosts.
    pub lock: Arc<tokio::sync::Mutex<()>>,
    pub jobs: Jobs,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/jobs/:id", get(get_job))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/:name", delete(delete_project))
        .route("/api/projects/:name/open-folder", post(open_folder))
        .route("/api/projects/:name/vscode", post(open_vscode))
        .route("/api/projects/:name/php", post(set_project_php))
        .route("/api/projects/:name/database", post(link_project_database))
        .route("/api/projects/:name/git-pull", post(git_pull))
        .route("/api/projects/:name/git-status", get(git_status))
        .route("/api/projects/:name/git-commit", post(git_commit))
        .route("/api/projects/:name/git-push", post(git_push))
        .route("/api/projects/:name/git-fetch", post(git_fetch))
        .route("/api/projects/:name/sql-files", get(project_sql_files))
        .route("/api/inspect-folder", post(inspect_folder))
        .route("/api/pick-folder", post(pick_folder))
        .route("/api/php/:version/remove", post(remove_php))
        .route("/api/backups", get(list_backups).post(create_backup))
        .route("/api/backups/import", post(import_backup))
        .route("/api/backups/:name", delete(delete_backup))
        .route("/api/backups/:name/download", get(download_backup))
        .route("/api/backups/:name/restore", post(restore_backup_config))
        .route("/api/projects/:name/:action", post(project_action))
        .route("/api/containers/:name/logs", get(container_logs))
        .route("/api/containers/:name/:action", post(container_action))
        .route("/api/databases", get(list_databases).post(create_database))
        .route("/api/databases/:name", delete(drop_database))
        .route("/api/infra/up", post(infra_up))
        .route("/api/updates", get(get_updates))
        .route("/api/updates/check", post(check_updates))
        .route("/api/updates/apply", post(apply_updates))
        .route("/api/hosts", get(list_hosts))
        .route("/api/hosts/entry/:domain", delete(delete_host))
        .route("/api/hosts/restore", post(restore_hosts))
        .route("/api/certs/install-ca", post(install_ca))
        .fallback(static_file)
        .layer(middleware::from_fn(local_guard))
        .with_state(state)
}

/// Blocks DNS-rebinding and cross-site requests: Host must be loopback and,
/// for state-changing requests, Origin (when present) must be the portal itself.
async fn local_guard(req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let hostname = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    if !matches!(hostname, "localhost" | "127.0.0.1" | "[::1]") {
        return (StatusCode::FORBIDDEN, "forbidden host").into_response();
    }
    if req.method() != Method::GET {
        if let Some(origin) = req.headers().get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
            if origin.split("://").nth(1) != Some(host) {
                return (StatusCode::FORBIDDEN, "forbidden origin").into_response();
            }
        }
    }
    next.run(req).await
}

async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref().to_string())], f.data.into_owned()).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn status(State(s): State<AppState>) -> Json<Value> {
    let docker_ok = s.docker.ping().await;
    let containers = if docker_ok { s.docker.status_all().await } else { vec![] };
    Json(json!({
        "docker": docker_ok,
        "docker_cli": docker::docker_cli_available(),
        "infra_ready": docker_ok && containers.iter().filter(|c| c.name == crate::config::PROXY_CONTAINER || c.name == crate::config::DB_CONTAINER).all(|c| c.running),
        "git_available": crate::git::available(),
        "vscode": crate::proc::vscode().is_some(),
        "services_down": docker_ok && !containers.iter().filter(|c| [crate::config::PROXY_CONTAINER, crate::config::DB_CONTAINER, crate::config::PMA_CONTAINER].contains(&c.name.as_str())).all(|c| c.running),
        "containers": containers,
        "mkcert": cert::mkcert_available(),
        "php_versions": PHP_VERSIONS,
        "php_support": crate::updates::php_support(),
        "hosts_file": s.cfg.hosts_path,
        "sites_dir": s.cfg.sites_dir,
        "database": {
            "host": "127.0.0.1",
            "port": 3306,
            "user": "root",
            "password": std::env::var("LDP_DB_ROOT_PASSWORD").unwrap_or_else(|_| "root".into()),
            "default_password": std::env::var("LDP_DB_ROOT_PASSWORD").is_err(),
            "phpmyadmin_url": "http://localhost:8080",
        },
        "bind": docker::bind_address(),
    }))
}

#[derive(Deserialize)]
struct NewDatabase {
    name: String,
}

fn validate_db_name(name: &str) -> AppResult<()> {
    if !projects::valid_ident(name, &['_']) {
        return Err(AppError::BadRequest("database name: use letters, digits and '_' (max 63 characters)".into()));
    }
    if docker::SYSTEM_DATABASES.iter().any(|d| d.eq_ignore_ascii_case(name)) {
        return Err(AppError::BadRequest("that name is reserved for a system database".into()));
    }
    Ok(())
}

async fn list_databases(State(s): State<AppState>) -> AppResult<Json<Value>> {
    let linked: Vec<(String, String)> = projects::load(&s.cfg)?
        .into_iter()
        .filter_map(|p| p.db_name.map(|d| (d, p.name)))
        .collect();
    let dbs = s.docker.list_databases().await.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let out: Vec<Value> = dbs
        .iter()
        .map(|d| {
            let mut v = serde_json::to_value(d).unwrap();
            v["project"] = json!(linked.iter().find(|(db, _)| db == &d.name).map(|(_, p)| p));
            v
        })
        .collect();
    Ok(Json(json!(out)))
}

async fn create_database(State(s): State<AppState>, Json(n): Json<NewDatabase>) -> AppResult<(StatusCode, Json<Value>)> {
    validate_db_name(&n.name)?;
    let title = format!("Creating database \"{}\"", n.name);
    Ok(spawn_job(&s, title, move |s, log| async move {
        s.docker.ensure_infra(&s.cfg, &log).await?;
        log.push(format!("Creating database `{}`...", n.name));
        s.docker.create_database(&n.name, &log).await?;
        Ok(())
    }))
}

async fn drop_database(State(s): State<AppState>, Path(name): Path<String>) -> AppResult<(StatusCode, Json<Value>)> {
    validate_db_name(&name)?;
    let title = format!("Dropping database \"{name}\"");
    Ok(spawn_job(&s, title, move |s, log| async move {
        log.push(format!("Dropping database `{name}`..."));
        s.docker.drop_database(&name, &log).await?;
        Ok(())
    }))
}

async fn list_projects(State(s): State<AppState>) -> AppResult<Json<Value>> {
    let list = projects::load(&s.cfg)?;
    let mut out: Vec<Value> = Vec::new();
    for p in &list {
        let dir = projects::site_path(&s.cfg, p);
        let mut v = serde_json::to_value(p).unwrap();
        v["ssl_ready"] = json!(cert::exists(&s.cfg.certs_dir(), &p.domain));
        v["path"] = json!(dir);
        v["web_root"] = json!(projects::web_root(&s.cfg, p));
        v["url"] = json!(format!("{}://{}", if p.ssl { "https" } else { "http" }, p.domain));
        v["git"] = json!(crate::git::info_cached(&dir).await);
        out.push(v);
    }
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
struct FolderInput {
    path: String,
}

/// Looks at a folder the user wants to use as a project: Git info, suggested name/docroot and SQL files.
async fn inspect_folder(Json(b): Json<FolderInput>) -> AppResult<Json<Value>> {
    let dir = std::path::PathBuf::from(b.path.trim());
    if !dir.is_absolute() || !dir.is_dir() {
        return Ok(Json(json!({ "exists": false })));
    }
    let git = crate::git::info(&dir).await;
    let suggested_name = dir.file_name().map(|n| crate::git::name_from_url(&n.to_string_lossy())).unwrap_or_default();
    let docroot = if dir.join("public").join("index.php").is_file() && !dir.join("index.php").is_file() { "public" } else { "" };
    let sql = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || projects::find_sql_files(&dir)
    })
    .await
    .unwrap_or_default();
    Ok(Json(json!({ "exists": true, "git": git, "suggested_name": suggested_name, "suggested_docroot": docroot, "sql_files": sql })))
}

/// Native folder picker (Windows); the browser cannot reveal full folder paths itself.
async fn pick_folder() -> AppResult<Json<Value>> {
    let path = tokio::task::spawn_blocking(crate::proc::pick_folder).await.map_err(anyhow::Error::from)?;
    match path {
        Ok(p) => Ok(Json(json!({ "path": p }))),
        Err(e) => Err(AppError::BadRequest(e)),
    }
}

async fn project_sql_files(State(s): State<AppState>, Path(name): Path<String>) -> AppResult<Json<Value>> {
    let p = projects::load(&s.cfg)?.into_iter().find(|p| p.name == name).ok_or_else(|| AppError::NotFound("project not found".into()))?;
    let dir = projects::site_path(&s.cfg, &p);
    let files = tokio::task::spawn_blocking(move || projects::find_sql_files(&dir)).await.unwrap_or_default();
    Ok(Json(json!(files)))
}

#[derive(Deserialize)]
struct LinkDatabase {
    db_name: String,
    sql_file: Option<String>,
}

async fn link_project_database(State(s): State<AppState>, Path(name): Path<String>, Json(b): Json<LinkDatabase>) -> (StatusCode, Json<Value>) {
    let title = format!("Database for \"{name}\"");
    spawn_job(&s, title, move |s, log| async move {
        projects::link_database(&s.cfg, &s.docker, &name, &b.db_name, b.sql_file.as_deref(), &log).await
    })
}

fn git_project_dir(s: &AppState, name: &str) -> AppResult<std::path::PathBuf> {
    let p = projects::load(&s.cfg)?.into_iter().find(|p| p.name == name).ok_or_else(|| AppError::NotFound("project not found".into()))?;
    let dir = projects::site_path(&s.cfg, &p);
    if !dir.join(".git").exists() {
        return Err(AppError::BadRequest("this project folder is not a Git repository".into()));
    }
    Ok(dir)
}

async fn git_status(State(s): State<AppState>, Path(name): Path<String>) -> AppResult<Json<Value>> {
    let dir = git_project_dir(&s, &name)?;
    let st = crate::git::status(&dir).await?;
    Ok(Json(json!(st)))
}

#[derive(Deserialize)]
struct GitCommitInput {
    message: String,
    /// None = commit all changes.
    paths: Option<Vec<String>>,
    #[serde(default)]
    push: bool,
}

async fn git_commit(State(s): State<AppState>, Path(name): Path<String>, Json(b): Json<GitCommitInput>) -> (StatusCode, Json<Value>) {
    let title = if b.push { format!("Git commit & push \"{name}\"") } else { format!("Git commit \"{name}\"") };
    spawn_job(&s, title, move |s, log| async move {
        let dir = git_project_dir(&s, &name)?;
        let res = async {
            crate::git::commit(&dir, &b.message, b.paths.as_deref(), &log).await?;
            if b.push {
                crate::git::push(&dir, &log).await?;
            }
            Ok(())
        }
        .await;
        crate::git::forget(&dir);
        res
    })
}

async fn git_push(State(s): State<AppState>, Path(name): Path<String>) -> (StatusCode, Json<Value>) {
    let title = format!("Git push \"{name}\"");
    spawn_job(&s, title, move |s, log| async move {
        let dir = git_project_dir(&s, &name)?;
        let res = crate::git::push(&dir, &log).await;
        crate::git::forget(&dir);
        res
    })
}

async fn git_fetch(State(s): State<AppState>, Path(name): Path<String>) -> (StatusCode, Json<Value>) {
    let title = format!("Git fetch \"{name}\"");
    spawn_job(&s, title, move |s, log| async move {
        let dir = git_project_dir(&s, &name)?;
        let res = crate::git::fetch(&dir, &log).await;
        crate::git::forget(&dir);
        res
    })
}

async fn git_pull(State(s): State<AppState>, Path(name): Path<String>) -> (StatusCode, Json<Value>) {
    let title = format!("Git pull \"{name}\"");
    spawn_job(&s, title, move |s, log| async move {
        let p = projects::load(&s.cfg)?.into_iter().find(|p| p.name == name).ok_or_else(|| AppError::NotFound("project not found".into()))?;
        let dir = projects::site_path(&s.cfg, &p);
        if !dir.join(".git").exists() {
            return Err(AppError::BadRequest("this project folder is not a Git repository".into()));
        }
        crate::git::pull(&dir, &log).await?;
        crate::git::forget(&dir);
        Ok(())
    })
}

/// Runs `f` as a background job (serialised by the global lock) and returns its id immediately.
/// The UI polls `/api/jobs/:id` to show progress in the overlay.
fn spawn_job<F, Fut>(s: &AppState, title: impl Into<String>, f: F) -> (StatusCode, Json<Value>)
where
    F: FnOnce(AppState, JobLog) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = AppResult<()>> + Send + 'static,
{
    let log = s.jobs.create(&title.into());
    let id = log.id;
    let st = s.clone();
    tokio::spawn(async move {
        let _guard = st.lock.lock().await;
        log.set_running();
        match f(st.clone(), log.clone()).await {
            Ok(()) => log.finish(None),
            Err(e) => {
                tracing::warn!("job {id} failed: {e}");
                log.push(format!("Error: {e}"));
                log.finish(Some(e.to_string()));
            }
        }
    });
    (StatusCode::ACCEPTED, Json(json!({ "job": id })))
}

async fn get_job(State(s): State<AppState>, Path(id): Path<u64>) -> AppResult<Json<Job>> {
    s.jobs.get(id).map(Json).ok_or_else(|| AppError::NotFound("job not found".into()))
}

async fn create_project(State(s): State<AppState>, Json(n): Json<projects::NewProject>) -> (StatusCode, Json<Value>) {
    let title = format!("Creating project \"{}\"", n.name);
    spawn_job(&s, title, move |s, log| async move {
        projects::create(&s.cfg, &s.docker, n, &log).await?;
        crate::updates::refresh_quietly(&s.cfg, &s.docker).await;
        Ok(())
    })
}

#[derive(serde::Deserialize)]
struct PhpChange {
    php_version: String,
}

async fn set_project_php(State(s): State<AppState>, Path(name): Path<String>, Json(b): Json<PhpChange>) -> (StatusCode, Json<Value>) {
    let title = format!("Switching \"{name}\" to PHP {}", b.php_version);
    spawn_job(&s, title, move |s, log| async move {
        projects::set_php_version(&s.cfg, &s.docker, &name, &b.php_version, &log).await?;
        crate::updates::refresh_quietly(&s.cfg, &s.docker).await;
        Ok(())
    })
}

async fn list_backups(State(s): State<AppState>) -> Json<crate::backup::BackupList> {
    Json(crate::backup::list(&s.cfg))
}

async fn create_backup(State(s): State<AppState>) -> (StatusCode, Json<Value>) {
    spawn_job(&s, "Creating configuration backup", |s, log| async move { crate::backup::create(&s.cfg, &log).map(|_| ()) })
}

#[derive(Deserialize)]
struct ImportBackup {
    content: String,
}

async fn import_backup(State(s): State<AppState>, Json(b): Json<ImportBackup>) -> (StatusCode, Json<Value>) {
    spawn_job(&s, "Importing configuration backup", move |s, log| async move { crate::backup::import(&s.cfg, &b.content, &log).map(|_| ()) })
}

async fn delete_backup(State(s): State<AppState>, Path(name): Path<String>) -> (StatusCode, Json<Value>) {
    let title = format!("Deleting backup {name}");
    spawn_job(&s, title, move |s, log| async move { crate::backup::delete(&s.cfg, &name, &log) })
}

async fn restore_backup_config(State(s): State<AppState>, Path(name): Path<String>) -> (StatusCode, Json<Value>) {
    let title = format!("Restoring backup {name}");
    spawn_job(&s, title, move |s, log| async move {
        crate::backup::restore(&s.cfg, &s.docker, &name, &log).await?;
        crate::updates::refresh_quietly(&s.cfg, &s.docker).await;
        Ok(())
    })
}

async fn download_backup(State(s): State<AppState>, Path(name): Path<String>) -> AppResult<Response> {
    let text = crate::backup::read_raw(&s.cfg, &name)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        text,
    )
        .into_response())
}

async fn remove_php(State(s): State<AppState>, Path(version): Path<String>) -> (StatusCode, Json<Value>) {
    let title = format!("Removing PHP {version}");
    spawn_job(&s, title, move |s, log| async move { crate::updates::remove_php(&s.cfg, &s.docker, &version, &log).await })
}

async fn delete_project(State(s): State<AppState>, Path(name): Path<String>) -> (StatusCode, Json<Value>) {
    let title = format!("Deleting project \"{name}\"");
    spawn_job(&s, title, move |s, log| async move {
        projects::delete(&s.cfg, &s.docker, &name, &log).await?;
        crate::updates::refresh_quietly(&s.cfg, &s.docker).await;
        Ok(())
    })
}

/// Opens the project folder in the OS file manager (only folders of known projects).
async fn open_folder(State(s): State<AppState>, Path(name): Path<String>) -> AppResult<Json<Value>> {
    let p = projects::load(&s.cfg)?.into_iter().find(|p| p.name == name).ok_or_else(|| AppError::NotFound("project not found".into()))?;
    let dir = projects::site_path(&s.cfg, &p);
    std::fs::create_dir_all(&dir)?;
    crate::proc::open_folder(&dir);
    Ok(Json(json!({ "ok": true })))
}

async fn open_vscode(State(s): State<AppState>, Path(name): Path<String>) -> AppResult<Json<Value>> {
    let p = projects::load(&s.cfg)?.into_iter().find(|p| p.name == name).ok_or_else(|| AppError::NotFound("project not found".into()))?;
    let dir = projects::site_path(&s.cfg, &p);
    if !dir.is_dir() {
        return Err(AppError::NotFound("project folder not found".into()));
    }
    if !crate::proc::open_in_vscode(&dir) {
        return Err(AppError::BadRequest("Visual Studio Code was not found".into()));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn project_action(State(s): State<AppState>, Path((name, action)): Path<(String, String)>) -> AppResult<(StatusCode, Json<Value>)> {
    if !matches!(action.as_str(), "start" | "stop" | "restart") {
        return Err(AppError::BadRequest("action must be start, stop or restart".into()));
    }
    let title = format!("{} project \"{name}\"", capitalize(&action));
    Ok(spawn_job(&s, title, move |s, log| async move {
        let enabled = action != "stop";
        let p = projects::set_enabled(&s.cfg, &s.docker, &name, enabled, &log).await?;
        if action == "restart" {
            log.push("Restarting PHP container...");
            s.docker.act(&crate::config::php_container(&p.php_version), Action::Restart).await?;
        }
        Ok(())
    }))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn parse_action(a: &str) -> AppResult<Action> {
    match a {
        "start" => Ok(Action::Start),
        "stop" => Ok(Action::Stop),
        "restart" => Ok(Action::Restart),
        _ => Err(AppError::BadRequest("action must be start, stop or restart".into())),
    }
}

async fn container_action(State(s): State<AppState>, Path((name, action)): Path<(String, String)>) -> AppResult<(StatusCode, Json<Value>)> {
    let act = parse_action(&action)?;
    DockerClient::check_managed(&name)?;
    let title = format!("{} {name}", capitalize(&action));
    Ok(spawn_job(&s, title, move |s, log| async move {
        if !s.docker.status(&name).await.exists && !matches!(act, Action::Stop) {
            // The container was never created: provision it instead of failing with a 404.
            log.push(format!("{name} does not exist yet; creating it."));
            match PHP_VERSIONS.iter().find(|v| crate::config::php_container(v) == name) {
                Some(v) => s.docker.ensure_php(&s.cfg, v, &log).await?,
                None => s.docker.ensure_infra(&s.cfg, &log).await?,
            }
        } else {
            log.push(format!("Running {action} on {name}..."));
            s.docker.act(&name, act).await?;
        }
        Ok(())
    }))
}

#[derive(Deserialize)]
struct LogQuery {
    tail: Option<usize>,
}

async fn container_logs(State(s): State<AppState>, Path(name): Path<String>, Query(q): Query<LogQuery>) -> AppResult<String> {
    s.docker.logs(&name, q.tail.unwrap_or(200).min(5000)).await
}

#[derive(Deserialize, Default)]
struct ApplyUpdates {
    #[serde(default)]
    components: Vec<String>,
}

/// Cached report of the last update check (null before the first check).
async fn get_updates(State(s): State<AppState>) -> Json<Value> {
    Json(json!(crate::updates::load_cached(&s.cfg)))
}

async fn check_updates(State(s): State<AppState>) -> (StatusCode, Json<Value>) {
    spawn_job(&s, "Checking for updates", move |s, log| async move {
        s.docker.ensure_docker_ready(&log).await?;
        crate::updates::check(&s.cfg, &s.docker, Some(&log)).await?;
        Ok(())
    })
}

async fn apply_updates(State(s): State<AppState>, body: Option<Json<ApplyUpdates>>) -> (StatusCode, Json<Value>) {
    let ids = body.map(|b| b.0.components).unwrap_or_default();
    let title = if ids.is_empty() { "Updating all software".to_string() } else { format!("Updating {}", ids.join(", ")) };
    spawn_job(&s, title, move |s, log| async move { crate::updates::apply(&s.cfg, &s.docker, &ids, &log).await })
}

async fn infra_up(State(s): State<AppState>) -> (StatusCode, Json<Value>) {
    spawn_job(&s, "Starting base services", |s, log| async move { s.docker.ensure_infra(&s.cfg, &log).await })
}

async fn list_hosts(State(s): State<AppState>) -> AppResult<Json<Value>> {
    let entries = hosts::list(&s.cfg.hosts_path)?;
    let backup = hosts::backup_path(&s.cfg.hosts_path);
    Ok(Json(json!({ "entries": entries, "backup": backup, "backup_exists": backup.exists() })))
}

/// Removes a managed hosts entry, refusing domains that belong to a registered project.
async fn delete_host(State(s): State<AppState>, Path(domain): Path<String>) -> AppResult<(StatusCode, Json<Value>)> {
    if projects::load(&s.cfg)?.iter().any(|p| p.domain == domain) {
        return Err(AppError::Conflict("domain belongs to a project; delete the project instead".into()));
    }
    let title = format!("Removing hosts entry {domain}");
    Ok(spawn_job(&s, title, move |s, log| async move {
        log.push("Updating the hosts file (Windows may ask for administrator permission)...");
        let p = s.cfg.hosts_path.clone();
        tokio::task::spawn_blocking(move || hosts::remove(&p, &domain)).await.map_err(anyhow::Error::from)?
    }))
}

async fn restore_hosts(State(s): State<AppState>) -> (StatusCode, Json<Value>) {
    spawn_job(&s, "Restoring hosts file backup", |s, log| async move {
        log.push("Restoring the hosts file (Windows may ask for administrator permission)...");
        let p = s.cfg.hosts_path.clone();
        tokio::task::spawn_blocking(move || hosts::restore_backup(&p)).await.map_err(anyhow::Error::from)?
    })
}

async fn install_ca(State(s): State<AppState>) -> (StatusCode, Json<Value>) {
    spawn_job(&s, "Installing mkcert root certificate", |_, log| async move {
        log.push("Running mkcert -install (Windows may show a security prompt)...");
        tokio::task::spawn_blocking(cert::install_ca).await.map_err(anyhow::Error::from)??;
        Ok(())
    })
}
