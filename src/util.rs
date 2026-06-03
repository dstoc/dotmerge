use anyhow::{Result, anyhow};
use std::ffi::OsStr;
use std::process::Output;

pub fn not_implemented(feature: &str) -> anyhow::Error {
    anyhow!("{feature} not implemented yet")
}

pub fn ensure_success(program: &str, args: &[impl AsRef<OsStr>], output: Output) -> Result<Output> {
    if output.status.success() {
        return Ok(output);
    }

    let rendered_args = args
        .iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();

    Err(anyhow!(
        "`{program} {rendered_args}` exited with {}{}",
        output.status,
        if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        }
    ))
}
