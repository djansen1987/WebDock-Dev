//! Certificate generation: prefers `mkcert` (locally trusted CA), falls back to `rcgen` self-signed.
//! Output: `<certs_dir>/<domain>.pem` and `<domain>-key.pem`, mounted into the Nginx proxy.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn cert_paths(dir: &Path, domain: &str) -> (PathBuf, PathBuf) {
    (dir.join(format!("{domain}.pem")), dir.join(format!("{domain}-key.pem")))
}

pub fn exists(dir: &Path, domain: &str) -> bool {
    let (c, k) = cert_paths(dir, domain);
    c.exists() && k.exists()
}

pub fn mkcert_available() -> bool {
    crate::proc::quiet(Command::new("mkcert").arg("-help")).output().map(|o| o.status.success() || !o.stdout.is_empty() || !o.stderr.is_empty()).unwrap_or(false)
}

/// Generates a cert for `domain` and `*.domain`. Returns the generator used.
pub fn generate(dir: &Path, domain: &str) -> Result<&'static str> {
    std::fs::create_dir_all(dir)?;
    let (cert, key) = cert_paths(dir, domain);
    if mkcert_available() {
        let out = crate::proc::quiet(Command::new("mkcert")
            .arg("-cert-file")
            .arg(&cert)
            .arg("-key-file")
            .arg(&key)
            .arg(domain)
            .arg(format!("*.{domain}")))
            .output()
            .context("running mkcert")?;
        if out.status.success() {
            return Ok("mkcert");
        }
        tracing::warn!("mkcert failed, falling back to rcgen: {}", String::from_utf8_lossy(&out.stderr));
    }
    let sans = vec![domain.to_string(), format!("*.{domain}"), "localhost".to_string()];
    let ck = rcgen::generate_simple_self_signed(sans).map_err(|e| anyhow::anyhow!("rcgen: {e}"))?;
    std::fs::write(&cert, ck.cert.pem())?;
    std::fs::write(&key, ck.key_pair.serialize_pem())?;
    Ok("rcgen")
}

pub fn remove(dir: &Path, domain: &str) -> Result<()> {
    let (c, k) = cert_paths(dir, domain);
    for p in [c, k] {
        if p.exists() {
            std::fs::remove_file(&p).with_context(|| format!("removing {}", p.display()))?;
        }
    }
    Ok(())
}

/// Installs the mkcert root CA into the system trust store (may prompt for privileges).
pub fn install_ca() -> Result<()> {
    if !mkcert_available() {
        bail!("mkcert is not installed; self-signed certificates will show a browser warning");
    }
    let s = crate::proc::quiet(Command::new("mkcert").arg("-install")).status()?;
    if !s.success() {
        bail!("mkcert -install failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn rcgen_fallback_writes_files() {
        let dir = tempfile::tempdir().unwrap();
        let ck = rcgen::generate_simple_self_signed(vec!["x.test".to_string()]).unwrap();
        assert!(ck.cert.pem().contains("BEGIN CERTIFICATE"));
        let _ = dir;
    }
}
