//! Pure hosts-file logic (no I/O): parsing, validation and rendering of the managed block.
//! Everything outside the `# BEGIN/END LOCAL DEV PORTAL` markers is never touched.

use crate::config::{MARKER_BEGIN, MARKER_END};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostEntry {
    pub ip: String,
    pub domain: String,
}

pub fn validate_domain(d: &str) -> Result<(), String> {
    if d.is_empty() || d.len() > 253 {
        return Err("domain must be 1-253 characters".into());
    }
    for label in d.split('.') {
        let ok = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !ok {
            return Err(format!("invalid domain label '{label}' (use lowercase a-z, 0-9, '-')"));
        }
    }
    Ok(())
}

pub fn validate_ip(ip: &str) -> Result<(), String> {
    ip.parse::<std::net::IpAddr>().map(|_| ()).map_err(|_| format!("invalid IP address '{ip}'"))
}

/// Entries currently inside the managed block.
pub fn parse_block(content: &str) -> Vec<HostEntry> {
    let mut inside = false;
    let mut out = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t == MARKER_BEGIN {
            inside = true;
        } else if t == MARKER_END {
            inside = false;
        } else if inside && !t.is_empty() && !t.starts_with('#') {
            let mut parts = t.split_whitespace();
            if let (Some(ip), Some(domain)) = (parts.next(), parts.next()) {
                out.push(HostEntry { ip: ip.into(), domain: domain.into() });
            }
        }
    }
    out
}

/// The file with the managed block (markers included) removed.
pub fn outside_block(content: &str) -> String {
    let mut inside = false;
    let mut out = String::new();
    for line in content.lines() {
        let t = line.trim();
        if t == MARKER_BEGIN {
            inside = true;
        } else if t == MARKER_END {
            inside = false;
        } else if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

/// Returns the line of an entry for `domain` outside the managed block (e.g. from another dev setup).
/// The first matching line wins in the OS resolver, so such an entry would shadow ours.
pub fn external_entry(content: &str, domain: &str) -> Option<String> {
    outside_block(content)
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find(|l| l.split('#').next().unwrap_or("").split_whitespace().skip(1).any(|h| h.eq_ignore_ascii_case(domain)))
        .map(str::to_string)
}

/// Returns `content` with the managed block replaced (or appended when absent).
pub fn render(content: &str, entries: &[HostEntry]) -> String {
    let nl = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = outside_block(content);
    if !entries.is_empty() {
        if !out.is_empty() {
            out.push('\n');
            out.push('\n');
        }
        out.push_str(MARKER_BEGIN);
        out.push('\n');
        for e in entries {
            out.push_str(&format!("{}\t{}\n", e.ip, e.domain));
        }
        out.push_str(MARKER_END);
    }
    out.push('\n');
    if nl == "\r\n" {
        out.replace('\n', "\r\n")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(d: &str) -> HostEntry {
        HostEntry { ip: "127.0.0.1".into(), domain: d.into() }
    }

    #[test]
    fn detects_entries_outside_block() {
        let src = "127.0.0.1 localhost\n192.168.13.155 dev.djqr.nl # old\n#1.2.3.4 other.nl\n\n# BEGIN LOCAL DEV PORTAL\n127.0.0.1\tmine.test\n# END LOCAL DEV PORTAL\n";
        assert!(external_entry(src, "dev.djqr.nl").is_some());
        assert!(external_entry(src, "other.nl").is_none());
        assert!(external_entry(src, "mine.test").is_none());
    }

    #[test]
    fn appends_block_and_preserves_rest() {
        let src = "127.0.0.1 localhost\n::1 localhost\n";
        let out = render(src, &[e("a.test")]);
        assert!(out.starts_with("127.0.0.1 localhost\n::1 localhost\n\n# BEGIN LOCAL DEV PORTAL\n"));
        assert_eq!(parse_block(&out), vec![e("a.test")]);
    }

    #[test]
    fn replaces_existing_block_idempotently() {
        let once = render("127.0.0.1 localhost\n", &[e("a.test"), e("b.test")]);
        let twice = render(&once, &[e("a.test"), e("b.test")]);
        assert_eq!(once, twice);
        let removed = render(&once, &[e("b.test")]);
        assert_eq!(parse_block(&removed), vec![e("b.test")]);
        assert_eq!(outside_block(&removed), "127.0.0.1 localhost");
    }

    #[test]
    fn empty_entries_removes_block() {
        let once = render("x\n", &[e("a.test")]);
        assert_eq!(render(&once, &[]), "x\n");
    }

    #[test]
    fn keeps_crlf() {
        let out = render("127.0.0.1 localhost\r\n", &[e("a.test")]);
        assert!(out.contains("\r\n") && !out.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn validation() {
        assert!(validate_domain("my-site.test").is_ok());
        assert!(validate_domain("bad domain").is_err());
        assert!(validate_domain("x\n127.0.0.1 evil").is_err());
        assert!(validate_ip("127.0.0.1").is_ok());
        assert!(validate_ip("nope").is_err());
    }
}
