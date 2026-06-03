use anyhow::anyhow;

pub fn not_implemented(feature: &str) -> anyhow::Error {
    anyhow!("{feature} not implemented yet")
}
