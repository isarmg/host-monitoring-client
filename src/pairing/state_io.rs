#[cfg(test)]
fn persist_state(config: &ClientConfig, state: &StoredPairingState) -> anyhow::Result<()> {
    let transaction = lock_state(config)?;
    let store = &transaction;
    persist_state_unlocked(store, state)
}

fn persist_state_unlocked(
    store: &StateTransaction,
    state: &StoredPairingState,
) -> anyhow::Result<()> {
    let mut output = sarmg_client_secret::SecretWriter::new(StateFile::Pairing.max_bytes())?;
    serde_json::to_writer_pretty(&mut output, state)?;
    let serialized = output.into_bytes();
    store
        .write(
            StateFile::Pairing,
            std::str::from_utf8(serialized.expose())?,
        )
        .context("failed to persist browser pairing state")
}

fn persist_active_binding_unlocked(
    config: &ClientConfig,
    store: &StateTransaction,
    binding: &ActiveBinding,
) -> anyhow::Result<()> {
    validate_active_binding(config, binding)?;
    store
        .write(StateFile::Binding, &serde_json::to_string_pretty(binding)?)
        .context("failed to persist active binding")
}

fn load_current_active_binding_unlocked(
    config: &ClientConfig,
    store: &StateReader,
    expected: &ActiveBinding,
) -> anyhow::Result<ActiveBinding> {
    match load_active_binding(config, store)? {
        Some(binding) if binding == *expected => Ok(binding),
        Some(_) => bail!("active binding does not match the current Active pairing state"),
        None => bail!("active binding is missing; purge local state and pair host-monitor again"),
    }
}

fn compare_and_persist_creating(
    config: &ClientConfig,
    generation: Uuid,
    pairing_endpoint: &str,
    report_endpoint: &str,
    polling_secret: &sarmg_client_secret::SecretString,
    next: &StoredPairingState,
) -> anyhow::Result<()> {
    let transaction = lock_state(config)?;
    let store = &transaction;
    let current = load_state(store)?;
    if !matches!(
        current,
        Some(StoredPairingState::Creating {
            generation: current_generation,
            pairing_endpoint: current_pairing_endpoint,
            report_endpoint: current_report_endpoint,
            polling_secret: current_polling_secret,
            ..
        }) if current_generation == generation
            && current_pairing_endpoint == pairing_endpoint
            && current_report_endpoint == report_endpoint
            && current_polling_secret.expose() == polling_secret.expose()
    ) {
        return Err(PairingSuperseded.into());
    }
    persist_state_unlocked(store, next)
}

fn compare_and_persist_pending(
    config: &ClientConfig,
    generation: Uuid,
    request_id: Uuid,
    pairing_endpoint: &str,
    report_endpoint: &str,
    polling_secret: &sarmg_client_secret::SecretString,
    next: &StoredPairingState,
) -> anyhow::Result<()> {
    let transaction = lock_state(config)?;
    let store = &transaction;
    ensure_pending_is_current(
        store,
        generation,
        request_id,
        pairing_endpoint,
        report_endpoint,
        polling_secret,
    )?;
    persist_state_unlocked(store, next)
}

fn ensure_pending_is_current(
    store: &StateReader,
    generation: Uuid,
    request_id: Uuid,
    pairing_endpoint: &str,
    report_endpoint: &str,
    polling_secret: &sarmg_client_secret::SecretString,
) -> anyhow::Result<()> {
    let current = load_state(store)?;
    if !matches!(
        current,
        Some(StoredPairingState::Pending {
            generation: current_generation,
            request_id: current_request_id,
            pairing_endpoint: current_pairing_endpoint,
            report_endpoint: current_report_endpoint,
            polling_secret: current_polling_secret,
            ..
        }) if current_generation == generation
            && current_request_id == request_id
            && current_pairing_endpoint == pairing_endpoint
            && current_report_endpoint == report_endpoint
            && current_polling_secret.expose() == polling_secret.expose()
    ) {
        return Err(PairingSuperseded.into());
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("browser pairing operation was superseded by a newer request; reloading saved state")]
struct PairingSuperseded;

fn validate_state_version(version: PairingStateVersion) -> anyhow::Result<()> {
    if version != PAIRING_STATE_VERSION {
        bail!("pairing state does not belong to the current Client package");
    }
    Ok(())
}
