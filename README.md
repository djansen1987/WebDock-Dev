# WebDock Dev

WebDock Dev is a single Rust binary with an embedded web UI (http://localhost:9000) and a Windows tray icon to manage local PHP projects (WordPress/Laravel/Symfony) through Docker: hosts file, SSL certificates, Nginx proxy, PHP 8.1-8.5, MariaDB and phpMyAdmin.

## For users (Windows)

1. Install [Docker Desktop](https://www.docker.com/products/docker-desktop/) and start it once.
2. Run `WebDockDev-Setup.exe` and follow the wizard (optional: desktop shortcut, start with Windows, mkcert).
3. Launch **WebDock Dev**. A tray icon appears and the dashboard opens. Click **New project**; the portal starts the proxy, database and the needed PHP version by itself (the first run downloads images and shows live progress).

Every action shows a progress overlay with live output. The dashboard is also a PWA: in Edge/Chrome click **Install app** in the header (or the install icon in the address bar) to get a standalone window and a desktop/Start menu shortcut. The portal itself must still be running (tray icon); the installed app shows a hint if it is not. Tray menu: *Open dashboard*, *Start with Windows*, *Quit*. Starting the app a second time just opens the dashboard.

**Where are my website files?** The installer asks for a website folder (default `Documents\WebDockDev\sites`, stored in `%LOCALAPPDATA%\WebDockDev\sites_dir.txt`; `LDP_SITES_DIR` overrides it). Each project lives in `<folder>\<project-name>`. The dashboard shows this path under every project, and **Open folder** opens it in Explorer. A new project gets a default `index.html` placeholder page (never overwritten if an index file exists). If you change the folder later, recreate the containers (`docker compose down` for the `ldp` project) so the new mount is used.

## For developers

```
cargo run                    # debug, console logging
.\build-installer.ps1        # release build + dist\WebDockDev-Setup.exe (needs Inno Setup 6)
cargo test
```

### Layout

```
Cargo.toml, build.rs         # build.rs embeds the Windows icon
src/
  main.rs                    # server thread + tray, `--hosts-helper`, `--background`
  config.rs                  # paths (data in %LOCALAPPDATA%\WebDockDev), container names
  jobs.rs                    # background jobs with live logs (drives the UI overlay)
  docker/                    # bollard: status, start/stop/restart, logs, exec; `docker compose` streaming
  hosts/                     # parser.rs (pure), mod.rs (backup, atomic write, CRUD), privilege.rs (elevation)
  cert/                      # mkcert, fallback rcgen
  projects/                  # projects.json, Nginx vhost generation, create/delete workflow with rollback
  api/                       # axum router, handlers, embedded UI, localhost guard
  tray.rs                    # Windows tray icon + autostart (HKCU Run)
web/                         # index.html + app.js (Tailwind via CDN)
docker/                      # docker-compose.yml + PHP Dockerfile (embedded in the binary)
installer/setup.iss          # Inno Setup script
```

Environment variables: `LDP_PORT`, `LDP_DATA_DIR`, `LDP_SITES_DIR`, `LDP_HOSTS_FILE`, `LDP_DB_ROOT_PASSWORD` (compose, default `root`).

For compatibility with installs made under the former name (Local Dev Portal), the `ldp-` container names, the `LDP_` variables, the compose project name and the hosts-file markers keep their original names. The data folder is `%LOCALAPPDATA%\WebDockDev`; if you used an older version, rename `%LOCALAPPDATA%\LocalDevPortal` to `WebDockDev` (with WebDock Dev closed) to keep your projects, certificates and backups.

### Jobs and the progress overlay

All state-changing endpoints return `202 {"job": id}`. The UI polls `GET /api/jobs/:id` (status, log lines, error) and shows them in the overlay. Jobs run one at a time.

## Permissions (hosts file)

The portal runs as a **normal user**. Only writing the hosts file needs elevated rights, through a minimal helper: `webdock-dev --hosts-helper <staged> <target>`.

- The portal first tries to write directly and only elevates on `PermissionDenied`.
- The helper **refuses** any change outside the `# BEGIN/END LOCAL DEV PORTAL` block, validates domains/IPs, writes `hosts.bak` and replaces the file atomically.

| OS | Mechanism | Notes |
|----|-----------|-------|
| Windows | PowerShell `Start-Process -Verb RunAs` | UAC prompt per change. To avoid prompts, grant your user Modify rights on `C:\Windows\System32\drivers\etc\hosts` (`icacls`). |
| Linux | `pkexec`, fallback `sudo -n` | Passwordless: sudoers rule `you ALL=(root) NOPASSWD: /path/to/webdock-dev --hosts-helper *`, or `setfacl -m u:$USER:rw /etc/hosts`. |
| macOS | `osascript ... with administrator privileges` | Native password dialog. |

Never run the portal itself as root/Administrator.

## Security

- Binds to loopback only; the `Host` header must be localhost/127.0.0.1 (DNS rebinding) and `Origin` of non-GET requests must be the portal itself (CSRF).
- Names, domains, database names and docroot are strictly validated (no injection into hosts/Nginx/SQL).
- Only `ldp-*` containers can be managed.
- For browser-trusted certificates install `mkcert` and click "Install mkcert CA"; otherwise self-signed certificates (rcgen) show a browser warning.
- Proxy, MariaDB and phpMyAdmin are published on `127.0.0.1` only. Set `LDP_BIND=0.0.0.0` to expose the proxy to your LAN (not recommended).
- The database root password defaults to `root` (local only). Set `LDP_DB_ROOT_PASSWORD` before first start to change it (put it in `<data>\.env`, see `.env.example`; on Windows `%LOCALAPPDATA%\WebDockDev\.env`); the dashboard warns while the default is used.

## Configuration backup

The **System** tab can create, download, import and restore configuration backups (`<data>\backups`, newest 10 kept). A backup contains the project list and certificates; hosts entries and Nginx vhosts are rebuilt on restore. Site files and databases are not included. Restoring first makes a safety backup of the current state. On opening the dashboard you are asked to create a new backup when the newest one is older than 30 days.

## Project sources: clone, existing folder, database import

**New project** offers three sources:

- **New empty**: a folder under the sites directory with a placeholder page.
- **Clone Git**: needs [Git](https://git-scm.com) on the PATH. Accepts `https://`, `ssh://`, `git@host:path` URLs or `owner/repo` (GitHub). Private repos work when Git can authenticate non-interactively (credential manager or SSH agent). Use the Pull button on the project row to update.
- **Existing folder**: any folder on your PC (Browse… on Windows). It is mounted into the containers at `/ext/<name>` through a generated compose override; files are never copied or moved, and deleting the project never deletes the folder. Git branch/remote are detected and shown.

If `public/index.php` exists, the document root is set to `public` automatically. Remote credentials are redacted in the UI.

**Databases:** enter a database name in the wizard (optionally with a `.sql` file to import), or use the database icon on a project row to create a database, link it and import a dump later. SQL files are found in the project folder or can be typed as a path. Only plain `.sql` files are supported (not `.gz`).

## Changing a project's PHP version

Use the PHP dropdown in the Projects table (or `POST /api/projects/:name/php` with `{"php_version":"8.4"}`). The portal starts that PHP container if needed, rewrites the vhost and reloads the proxy; your files are untouched. PHP versions that no project uses show a "Remove" button in the Software & security table (`POST /api/php/:version/remove`); the container is recreated automatically when needed again.

## Docker Hub rate limit

Anonymous Docker Hub pulls are limited (100 per hour per IP). The first install pulls many images, so sign in to Docker Desktop with a free account first (or run `docker login`). If a job fails with "Docker Hub rate limit reached", sign in or wait an hour and retry.

## Software updates

The "Software & security" section lists Nginx, MariaDB, phpMyAdmin and each PHP version with installed/latest version, support status (from endoflife.date) and whether a newer image exists. It is checked once a day automatically (or via "Check for updates"), and a banner appears when something is end of life or outdated. "Update" pulls images (PHP images are rebuilt), recreates the container and keeps your files and databases.
