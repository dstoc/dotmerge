use anyhow::{anyhow, Context};
use std::path::PathBuf;

pub(crate) fn hostname_label() -> String {
    read_hostname()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown host".to_string())
}

/// Read the kernel's current hostname via `gethostname(2)`.
///
/// This reflects the running hostname (what `hostname` reports), unlike the
/// `$HOSTNAME` shell variable — which is unset outside interactive bash — or
/// `/etc/hostname`, which is the static configured name and may be absent or
/// stale. Returns `None` if the syscall fails.
fn read_hostname() -> Option<String> {
    // POSIX bounds HOST_NAME_MAX at 255; a 256-byte buffer always leaves room
    // for the trailing NUL, which we keep reserved so the result is terminated
    // even when the name is truncated.
    let mut buf = vec![0u8; 256];
    // SAFETY: `buf` is a valid, writable allocation of `buf.len()` bytes. We
    // pass `len - 1` so `gethostname` never writes the final byte, which stays
    // NUL and guarantees a terminator regardless of truncation.
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len() - 1) };
    if ret != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

pub(crate) fn home_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("$HOME is not set"))?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err(anyhow!(
            "$HOME must be an absolute path, got `{}`",
            home.display()
        ));
    }

    std::fs::canonicalize(&home)
        .with_context(|| format!("failed to canonicalize `$HOME` at `{}`", home.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_label_is_sane() {
        // On any configured system gethostname succeeds, so the label is a
        // trimmed, NUL-free, non-empty string rather than the fallback.
        let label = hostname_label();
        assert!(!label.is_empty());
        assert!(!label.contains('\0'));
        assert_eq!(label, label.trim());
        assert_ne!(label, "unknown host");
    }
}
