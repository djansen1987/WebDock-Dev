//! Helpers for spawning child processes without flashing console windows (Windows GUI build).

/// Hides the console window of a child process on Windows; no-op elsewhere.
pub fn quiet(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

pub fn quiet_tokio(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Opens a folder in the OS file manager.
pub fn open_folder(dir: &std::path::Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(dir).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(dir).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

/// Shows the native "select folder" dialog and returns the chosen path (None when cancelled).
/// Windows only; other systems get an error and the UI falls back to typing the path.
pub fn pick_folder() -> Result<Option<String>, String> {
    #[cfg(windows)]
    {
        let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; \
            $o=New-Object System.Windows.Forms.Form -Property @{TopMost=$true}; \
            $f=New-Object System.Windows.Forms.FolderBrowserDialog; $f.Description='Select the project folder'; \
            if($f.ShowDialog($o) -eq 'OK'){[Console]::Out.Write($f.SelectedPath)}";
        let out = quiet(std::process::Command::new("powershell").args(["-NoProfile", "-STA", "-Command", script]))
            .output()
            .map_err(|e| format!("could not open the folder dialog: {e}"))?;
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok((!p.is_empty()).then_some(p))
    }
    #[cfg(not(windows))]
    {
        Err("The folder dialog is only available on Windows. Type the full path instead.".into())
    }
}

/// Location of the VS Code executable, if installed (cached; looked up once).
pub fn vscode() -> Option<&'static std::path::PathBuf> {
    static CACHE: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    CACHE.get_or_init(find_vscode).as_ref()
}

fn find_vscode() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    #[cfg(windows)]
    {
        let mut candidates: Vec<PathBuf> = Vec::new();
        for var in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base);
                let sub = if var == "LOCALAPPDATA" { base.join("Programs") } else { base };
                candidates.push(sub.join("Microsoft VS Code").join("Code.exe"));
            }
        }
        // `code.cmd` lives in <install>\bin, next to Code.exe one level up.
        for d in &path_dirs {
            if d.join("code.cmd").is_file() {
                if let Some(root) = d.parent() {
                    candidates.push(root.join("Code.exe"));
                }
            }
        }
        candidates.into_iter().find(|c| c.is_file())
    }
    #[cfg(not(windows))]
    {
        path_dirs.iter().map(|d| d.join("code")).find(|c| c.is_file())
    }
}

/// Opens a folder in VS Code (a new window unless it is already open).
pub fn open_in_vscode(dir: &std::path::Path) -> bool {
    match vscode() {
        Some(exe) => std::process::Command::new(exe).arg(dir).spawn().is_ok(),
        None => false,
    }
}

/// Opens a URL in the default browser.
pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        let _ = quiet(std::process::Command::new("cmd").args(["/C", "start", "", url])).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}
