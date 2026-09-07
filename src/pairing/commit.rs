use std::path::PathBuf;

use crate::config::ClientConfig;

pub(super) fn apply_active_config(config: &mut ClientConfig, report_endpoint: &str) {
    config.endpoint = report_endpoint.to_string();
    config.pairing_endpoint = None;
}

pub(super) fn persist_active_config_unlocked(
    config: &ClientConfig,
    report_endpoint: &str,
) -> anyhow::Result<PathBuf> {
    let mut active = config.clone();
    apply_active_config(&mut active, report_endpoint);
    active.persist_after_pairing()
}
