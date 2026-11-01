//! Docker Engine integration through `bollard` (unix socket / Windows named pipe).

use crate::config::{managed_containers, Config, DB_CONTAINER, PMA_CONTAINER, PROXY_CONTAINER};
use crate::error::{AppError, AppResult};
use crate::jobs::JobLog;
use anyhow::{Context, Result};
use bollard::container::{
    InspectContainerOptions, LogsOptions, RestartContainerOptions, StartContainerOptions, StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::Docker;
use futures_util::StreamExt;
use serde::Serialize;
use std::path::Path;

#[derive(Clone)]
pub struct DockerClient {
    docker: Docker,
}

#[derive(Debug, Serialize)]
pub struct ContainerStatus {
    pub name: String,
    pub exists: bool,
    pub running: bool,
    pub status: String,
    pub image: Option<String>,
    pub started_at: Option<String>,
}

pub const SYSTEM_DATABASES: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

#[derive(Debug, Serialize)]
pub struct DatabaseInfo {
    pub name: String,
    pub tables: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum Action {
    Start,
    Stop,
    Restart,
}

impl DockerClient {
    pub fn connect() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults().context("connecting to Docker")?;
        Ok(Self { docker })
    }

    pub async fn ping(&self) -> bool {
        self.docker.ping().await.is_ok()
    }

    /// Only containers managed by this portal may be touched.
    pub fn check_managed(name: &str) -> AppResult<()> {
        if managed_containers().iter().any(|c| c == name) {
            Ok(())
        } else {
            Err(AppError::BadRequest(format!("'{name}' is not a managed container")))
        }
    }

    pub async fn status(&self, name: &str) -> ContainerStatus {
        match self.docker.inspect_container(name, None::<InspectContainerOptions>).await {
            Ok(info) => {
                let state = info.state.unwrap_or_default();
                ContainerStatus {
                    name: name.into(),
                    exists: true,
                    running: state.running.unwrap_or(false),
                    status: state.status.map(|s| s.to_string()).unwrap_or_else(|| "unknown".into()),
                    image: info.config.and_then(|c| c.image),
                    started_at: state.started_at,
                }
            }
            Err(_) => ContainerStatus {
                name: name.into(),
                exists: false,
                running: false,
                status: "not created".into(),
                image: None,
                started_at: None,
            },
        }
    }

    pub async fn status_all(&self) -> Vec<ContainerStatus> {
        let mut v = Vec::new();
        for n in managed_containers() {
            v.push(self.status(&n).await);
        }
        v
    }

    pub async fn act(&self, name: &str, action: Action) -> AppResult<()> {
        Self::check_managed(name)?;
        let r = match action {
            Action::Start => self.docker.start_container(name, None::<StartContainerOptions<String>>).await,
            Action::Stop => self.docker.stop_container(name, None::<StopContainerOptions>).await,
            Action::Restart => self.docker.restart_container(name, None::<RestartContainerOptions>).await,
        };
        r.map_err(|e| AppError::Internal(anyhow::anyhow!("docker {action:?} {name}: {e}")))
    }

    /// Last `tail` log lines (stdout + stderr) of a managed container.
    pub async fn logs(&self, name: &str, tail: usize) -> AppResult<String> {
        Self::check_managed(name)?;
        let opts = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            timestamps: true,
            tail: tail.to_string(),
            ..Default::default()
        };
        let mut stream = self.docker.logs(name, Some(opts));
        let mut out = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => out.push_str(&String::from_utf8_lossy(&chunk.into_bytes())),
                Err(e) => return Err(AppError::NotFound(format!("logs for {name}: {e}"))),
            }
        }
        Ok(out)
    }

    /// Runs a command inside a managed container; returns (exit code, combined output).
    pub async fn exec(&self, name: &str, cmd: Vec<&str>) -> Result<(i64, String)> {
        let exec = self
            .docker
            .create_exec(
                name,
                CreateExecOptions {
                    cmd: Some(cmd.into_iter().map(String::from).collect()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await?;
        let mut output = String::new();
        if let StartExecResults::Attached { output: mut s, .. } = self.docker.start_exec(&exec.id, None).await? {
            while let Some(Ok(chunk)) = s.next().await {
                output.push_str(&String::from_utf8_lossy(&chunk.into_bytes()));
            }
        }
        let code = self.docker.inspect_exec(&exec.id).await?.exit_code.unwrap_or(-1);
        Ok((code, output))
    }

    /// Validates and reloads the Nginx configuration of the global proxy.
    pub async fn reload_proxy(&self) -> Result<()> {
        let (code, out) = self.exec(PROXY_CONTAINER, vec!["nginx", "-t"]).await?;
        if code != 0 {
            anyhow::bail!("nginx config test failed: {out}");
        }
        let (code, out) = self.exec(PROXY_CONTAINER, vec!["nginx", "-s", "reload"]).await?;
        if code != 0 {
            anyhow::bail!("nginx reload failed: {out}");
        }
        Ok(())
    }

    /// Waits until MariaDB accepts connections (it needs several seconds on first start).
    pub async fn wait_for_database(&self, log: &JobLog) -> Result<()> {
        let script = "mariadb-admin ping -uroot -p\"$MARIADB_ROOT_PASSWORD\" --silent";
        for attempt in 0..60 {
            if let Ok((0, _)) = self.exec(DB_CONTAINER, vec!["sh", "-c", script]).await {
                return Ok(());
            }
            if attempt == 0 {
                log.push("Waiting for MariaDB to accept connections...");
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        anyhow::bail!("MariaDB did not become ready within 2 minutes")
    }

    /// Image name and environment of a container (works for stopped containers); None if it does not exist.
    pub async fn container_info(&self, name: &str) -> Option<(String, Vec<String>)> {
        let info = self.docker.inspect_container(name, None::<InspectContainerOptions>).await.ok()?;
        let cfg = info.config?;
        Some((cfg.image.unwrap_or_default(), cfg.env.unwrap_or_default()))
    }

    /// Registry digests ("repo@sha256:...") of a local image; empty if the image is unknown.
    pub async fn image_digests(&self, image: &str) -> Vec<String> {
        self.docker.inspect_image(image).await.ok().and_then(|i| i.repo_digests).unwrap_or_default()
    }

    /// True when the proxy publishes its ports on the interface currently configured.
    async fn proxy_bind_current(&self) -> bool {
        let want = bind_address();
        let Ok(info) = self.docker.inspect_container(PROXY_CONTAINER, None::<InspectContainerOptions>).await else {
            return false;
        };
        let bindings = info.host_config.and_then(|h| h.port_bindings).unwrap_or_default();
        ["80/tcp", "443/tcp"].iter().all(|p| {
            bindings.get(*p).and_then(|b| b.as_ref()).map_or(false, |v| v.iter().all(|b| b.host_ip.as_deref() == Some(want.as_str())))
        })
    }

    async fn has_env(&self, name: &str, entry: &str) -> bool {
        match self.docker.inspect_container(name, None::<InspectContainerOptions>).await {
            Ok(info) => info.config.and_then(|c| c.env).unwrap_or_default().iter().any(|e| e == entry),
            Err(_) => false,
        }
    }

    /// Lists databases with table count and size in bytes.
    pub async fn list_databases(&self) -> Result<Vec<DatabaseInfo>> {
        let sql = "SELECT s.schema_name, COUNT(t.table_name), COALESCE(SUM(t.data_length + t.index_length), 0) \
                   FROM information_schema.schemata s LEFT JOIN information_schema.tables t ON t.table_schema = s.schema_name \
                   WHERE s.schema_name NOT IN (\"information_schema\", \"mysql\", \"performance_schema\", \"sys\") \
                   GROUP BY s.schema_name ORDER BY s.schema_name";
        let script = format!("mariadb -uroot -p\"$MARIADB_ROOT_PASSWORD\" -N -B -e '{sql}'");
        let (code, out) = self.exec(DB_CONTAINER, vec!["sh", "-c", &script]).await?;
        if code != 0 {
            anyhow::bail!("listing databases failed: {out}");
        }
        Ok(out
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                let (name, tables, bytes) = (f.next()?, f.next()?.parse().ok()?, f.next()?.trim().parse().ok()?);
                Some(DatabaseInfo { name: name.to_string(), tables, bytes })
            })
            .collect())
    }

    /// Drops a database (name must be pre-validated and must not be a system database).
    pub async fn drop_database(&self, db: &str, log: &JobLog) -> Result<()> {
        self.wait_for_database(log).await?;
        let script = format!("mariadb -uroot -p\"$MARIADB_ROOT_PASSWORD\" -e 'DROP DATABASE IF EXISTS `{db}`'");
        let (code, out) = self.exec(DB_CONTAINER, vec!["sh", "-c", &script]).await?;
        if code != 0 {
            anyhow::bail!("dropping database failed: {out}");
        }
        Ok(())
    }

    /// Creates the database if it does not exist (name must be pre-validated: [A-Za-z0-9_]+).
    pub async fn create_database(&self, db: &str, log: &JobLog) -> Result<()> {
        self.wait_for_database(log).await?;
        let sql = format!("CREATE DATABASE IF NOT EXISTS `{db}` CHARACTER SET utf8mb4");
        let script = format!("mariadb -uroot -p\"$MARIADB_ROOT_PASSWORD\" -e '{sql}'");
        let (code, out) = self.exec(DB_CONTAINER, vec!["sh", "-c", &script]).await?;
        if code != 0 {
            anyhow::bail!("creating database failed: {out}");
        }
        Ok(())
    }

    /// Imports a .sql file into `db` by streaming it into the mariadb client inside the database container.
    pub async fn import_sql(&self, db: &str, file: &Path, log: &JobLog) -> AppResult<()> {
        use std::process::Stdio;
        self.wait_for_database(log).await?;
        let size = std::fs::metadata(file)?.len();
        log.push(format!("Importing {} ({}) into `{db}`... large files can take a while.", file.display(), human_size(size)));
        let script = format!("mariadb -uroot -p\"$MARIADB_ROOT_PASSWORD\" --default-character-set=utf8mb4 {db}");
        let mut cmd = tokio::process::Command::new("docker");
        crate::proc::quiet_tokio(&mut cmd);
        cmd.args(["exec", "-i", DB_CONTAINER, "sh", "-c", &script])
            .stdin(Stdio::from(std::fs::File::open(file)?))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let out = tokio::time::timeout(std::time::Duration::from_secs(3600), cmd.output())
            .await
            .map_err(|_| AppError::BadRequest("SQL import timed out after 1 hour".into()))?
            .map_err(|e| AppError::BadRequest(format!("could not run docker: {e}")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let msg: String = err.lines().filter(|l| !l.contains("Using a password")).take(3).collect::<Vec<_>>().join(" ");
            return Err(AppError::BadRequest(format!("SQL import failed: {msg}")));
        }
        log.push("SQL import finished.");
        Ok(())
    }

    /// True when the container's bind mounts point at the folders the portal currently uses.
    /// Mismatches happen when the data or sites folder changed after the container was created.
    async fn mounts_current(&self, name: &str, expected: &[(&str, String)]) -> bool {
        let Ok(info) = self.docker.inspect_container(name, None::<InspectContainerOptions>).await else {
            return false;
        };
        let mounts = info.mounts.unwrap_or_default();
        let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_ascii_lowercase();
        expected.iter().all(|(dest, src)| {
            mounts.iter().any(|m| m.destination.as_deref() == Some(*dest) && m.source.as_deref().map(norm) == Some(norm(src)))
        })
    }

    /// Makes sure Docker is reachable and the base services (proxy, MariaDB, phpMyAdmin) exist and run.
    pub async fn ensure_infra(&self, cfg: &Config, log: &JobLog) -> AppResult<()> {
        self.ensure_docker(log).await?;
        let running = self.status(PROXY_CONTAINER).await.running
            && self.status(DB_CONTAINER).await.running
            && self.status(PMA_CONTAINER).await.running;
        let mut expected = vec![
            ("/var/www", cfg.sites_dir.to_string_lossy().to_string()),
            ("/etc/nginx/certs", cfg.certs_dir().to_string_lossy().to_string()),
            ("/etc/nginx/conf.d", cfg.nginx_conf_dir().to_string_lossy().to_string()),
        ];
        let custom = crate::projects::custom_mounts(cfg);
        expected.extend(custom.iter().map(|(dest, src)| (dest.as_str(), src.clone())));
        let current = running
            && self.mounts_current(PROXY_CONTAINER, &expected).await
            && self.proxy_bind_current().await
            && self.has_env(PMA_CONTAINER, "PMA_USER=root").await;
        if current {
            log.push("Base services are already running.");
        } else {
            if running {
                log.push("The proxy uses different folders than the current settings; recreating it.");
            }
            log.push("==> Starting base services (proxy, MariaDB, phpMyAdmin)");
            log.push("The first run downloads images and can take several minutes.");
            compose_up(cfg, &["proxy", "mariadb", "phpmyadmin"], log).await?;
        }
        Ok(())
    }

    /// Makes sure the FPM container for a PHP version exists and runs (image is built on first use).
    pub async fn ensure_php(&self, cfg: &Config, version: &str, log: &JobLog) -> AppResult<()> {
        self.ensure_docker(log).await?;
        let name = crate::config::php_container(version);
        let mut expected = vec![("/var/www", cfg.sites_dir.to_string_lossy().to_string())];
        let custom = crate::projects::custom_mounts(cfg);
        expected.extend(custom.iter().map(|(dest, src)| (dest.as_str(), src.clone())));
        if self.status(&name).await.running && self.mounts_current(&name, &expected).await {
            log.push(format!("PHP {version} is already running."));
        } else {
            if self.status(&name).await.exists {
                log.push(format!("==> Starting PHP {version}"));
            } else {
                log.push(format!("==> Installing PHP {version} (first use: downloading and building the image)"));
                log.push("This can take a few minutes; the project is created afterwards.");
            }
            let svc = format!("php{}", version.replace('.', ""));
            compose_up(cfg, &[svc.as_str()], log).await?;
        }
        Ok(())
    }

    pub async fn ensure_docker_ready(&self, log: &JobLog) -> AppResult<()> {
        self.ensure_docker(log).await
    }

    async fn ensure_docker(&self, log: &JobLog) -> AppResult<()> {
        log.push("Checking Docker...");
        if !docker_cli_available() {
            return Err(AppError::BadRequest("Docker is not installed (docker CLI not found). Install Docker Desktop first.".into()));
        }
        if !self.ping().await {
            return Err(AppError::BadRequest("Docker is not running. Start Docker Desktop and wait until it reports 'running'.".into()));
        }
        Ok(())
    }
}

fn human_size(b: u64) -> String {
    if b < 1024 * 1024 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else {
        format!("{:.1} MB", b as f64 / 1048576.0)
    }
}

/// Interface the proxy publishes ports 80/443 on. Loopback by default so sites are not exposed to the LAN;
/// set `LDP_BIND=0.0.0.0` to share them.
pub fn bind_address() -> String {
    std::env::var("LDP_BIND").ok().filter(|b| !b.trim().is_empty()).unwrap_or_else(|| "127.0.0.1".into())
}

/// Runs a plain `docker <args>` command; returns trimmed stdout, or None on failure/timeout.
pub async fn docker_cli(args: &[&str], timeout_secs: u64) -> Option<String> {
    let mut cmd = tokio::process::Command::new("docker");
    crate::proc::quiet_tokio(&mut cmd);
    cmd.args(args).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), cmd.output()).await.ok()?.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

const COMPOSE_YML: &str = include_str!("../../docker/docker-compose.yml");
const PHP_DOCKERFILE: &str = include_str!("../../docker/php/Dockerfile");

pub fn docker_cli_available() -> bool {
    let mut cmd = std::process::Command::new("docker");
    crate::proc::quiet(&mut cmd)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Writes the embedded compose file + PHP Dockerfile into `<data>/docker`, so the binary is self-contained.
fn write_compose_files(cfg: &Config) -> Result<std::path::PathBuf> {
    let dir = cfg.data_dir.join("docker");
    std::fs::create_dir_all(dir.join("php"))?;
    std::fs::write(dir.join("docker-compose.yml"), COMPOSE_YML)?;
    std::fs::write(dir.join("php").join("Dockerfile"), PHP_DOCKERFILE)?;
    Ok(dir.join("docker-compose.yml"))
}

/// Bind mounts for projects outside the sites directory, merged into the proxy and PHP services.
fn write_override(cfg: &Config, dir: &Path) -> Result<std::path::PathBuf> {
    let mounts = crate::projects::custom_mounts(cfg);
    let mut yml = String::from("# Generated by WebDock Dev - do not edit\n");
    if mounts.is_empty() {
        yml.push_str("services: {}\n");
    } else {
        yml.push_str("services:\n");
        let mut services = vec!["proxy".to_string()];
        services.extend(crate::config::PHP_VERSIONS.iter().map(|v| format!("php{}", v.replace('.', ""))));
        for svc in services {
            yml.push_str(&format!("  {svc}:\n    volumes:\n"));
            for (dest, src) in &mounts {
                // JSON strings are valid YAML double-quoted scalars.
                yml.push_str(&format!(
                    "      - type: bind\n        source: {}\n        target: {}\n",
                    serde_json::to_string(&src.replace('\\', "/"))?,
                    serde_json::to_string(dest)?
                ));
            }
        }
    }
    let file = dir.join("docker-compose.custom.yml");
    std::fs::write(&file, yml)?;
    Ok(file)
}

/// `docker compose up -d --build <services>` with live output streamed into the job log
/// (bollard has no compose support).
pub async fn compose_up(cfg: &Config, services: &[&str], log: &JobLog) -> Result<()> {
    let mut args = vec!["up", "-d", "--build"];
    args.extend(services);
    compose(cfg, &args, log).await
}

/// Runs `docker compose <args>` for the portal's project with live output streamed into the job log.
pub async fn compose(cfg: &Config, args: &[&str], log: &JobLog) -> Result<()> {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

    std::fs::create_dir_all(cfg.certs_dir())?;
    std::fs::create_dir_all(cfg.nginx_conf_dir())?;
    std::fs::create_dir_all(&cfg.sites_dir)?;
    let file = write_compose_files(cfg)?;
    let custom = write_override(cfg, file.parent().unwrap_or(Path::new(".")))?;
    // Forward slashes avoid drive-letter/backslash problems in compose volume strings.
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/");

    let mut cmd = tokio::process::Command::new("docker");
    crate::proc::quiet_tokio(&mut cmd);
    cmd.args(["compose", "--progress", "plain", "-p", "local-dev-portal", "-f"])
        .arg(&file)
        .arg("-f")
        .arg(&custom)
        .args(args)
        .env("LDP_DATA_DIR", norm(&cfg.data_dir))
        .env("LDP_SITES_DIR", norm(&cfg.sites_dir))
        .env("LDP_BIND", bind_address())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!("Docker CLI not found in PATH. Install Docker Desktop and restart the portal.")
        } else {
            anyhow::Error::from(e).context("running `docker compose`")
        }
    })?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    fn pump<R: AsyncRead + Unpin + Send + 'static>(r: R, tx: tokio::sync::mpsc::UnboundedSender<String>) {
        tokio::spawn(async move {
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let _ = tx.send(l);
            }
        });
    }
    pump(child.stdout.take().context("stdout")?, tx.clone());
    pump(child.stderr.take().context("stderr")?, tx);

    let mut tail = std::collections::VecDeque::new();
    while let Some(line) = rx.recv().await {
        if tail.len() == 15 {
            tail.pop_front();
        }
        tail.push_back(line.clone());
        log.push(line);
    }
    let status = child.wait().await?;
    if !status.success() {
        let tail = tail.into_iter().collect::<Vec<_>>().join("\n");
        if tail.contains("429") || tail.contains("toomanyrequests") {
            anyhow::bail!(
                "Docker Hub rate limit reached (too many anonymous pulls). Sign in to Docker Desktop with a free Docker account (or run `docker login`) and try again, or wait a few hours.\n{tail}"
            );
        }
        anyhow::bail!("docker compose failed:\n{tail}");
    }
    Ok(())
}
