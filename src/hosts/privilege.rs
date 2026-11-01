//! Cross-platform privilege escalation for the tiny "write hosts file" helper.
//!
//! The portal itself runs as a normal user. Only the helper invocation
//! (`webdock-dev --hosts-helper <staged> <target>`) is elevated:
//! - Linux:   `pkexec` (graphical polkit prompt), fallback `sudo -n` (e.g. sudoers NOPASSWD rule)
//! - macOS:   `osascript ... with administrator privileges` (native password dialog)
//! - Windows: PowerShell `Start-Process -Verb RunAs` (UAC prompt)

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

fn safe(p: &Path) -> Result<String> {
    let s = p.to_str().context("non-UTF8 path")?.to_string();
    if s.contains(['"', '\'', '`', '$', '\n', '%']) {
        bail!("unsafe characters in path: {s}");
    }
    Ok(s)
}

pub fn run_elevated_helper(staged: &Path, target: &Path) -> Result<()> {
    let exe = safe(&std::env::current_exe()?)?;
    let staged = safe(staged)?;
    let target = safe(target)?;

    #[cfg(target_os = "windows")]
    let status = {
        let args = format!("--hosts-helper \"{staged}\" \"{target}\"");
        let script = format!(
            "$p = Start-Process -FilePath '{exe}' -ArgumentList '{}' -Verb RunAs -Wait -PassThru -WindowStyle Hidden; exit $p.ExitCode",
            args.replace('\'', "''")
        );
        crate::proc::quiet(Command::new("powershell").args(["-NoProfile", "-Command", &script])).status()?
    };

    #[cfg(target_os = "macos")]
    let status = {
        let cmd = format!("\\\"{exe}\\\" --hosts-helper \\\"{staged}\\\" \\\"{target}\\\"");
        let script = format!("do shell script \"{cmd}\" with administrator privileges");
        Command::new("osascript").args(["-e", &script]).status()?
    };

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let status = {
        let args = ["--hosts-helper", staged.as_str(), target.as_str()];
        let via_pkexec = Command::new("pkexec").arg(&exe).args(args).status();
        match via_pkexec {
            Ok(s) => s,
            Err(_) => Command::new("sudo").args(["-n", &exe]).args(args).status()?,
        }
    };

    if !status.success() {
        let err_file = format!("{staged}.err");
        let reason = std::fs::read_to_string(&err_file).ok().filter(|s| !s.trim().is_empty());
        let _ = std::fs::remove_file(&err_file);
        match reason {
            Some(r) => bail!("hosts helper failed: {}", r.trim()),
            None => bail!(
                "administrator permission was not granted (the Windows/OS prompt was cancelled, missed or timed out; {status}). \
                 Try again and approve the prompt, or run WebDock Dev once as administrator"
            ),
        }
    }
    Ok(())
}
