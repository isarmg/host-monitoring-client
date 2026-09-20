#[cfg(test)]
fn state_path(config: &ClientConfig) -> PathBuf {
    config.state_dir.join(PAIRING_STATE_FILE)
}

#[cfg(test)]
fn active_binding_path(config: &ClientConfig) -> PathBuf {
    config.state_dir.join(ACTIVE_BINDING_FILE)
}

fn load_active_binding(
    config: &ClientConfig,
    store: &StateReader,
) -> anyhow::Result<Option<ActiveBinding>> {
    let path = store.path(StateFile::Binding);
    let bytes = match store.read(StateFile::Binding) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read active binding {}", path.display()));
        }
    };
    let binding: ActiveBinding = decode_pairing_document(&bytes, "active-binding")?;
    validate_active_binding(config, &binding)
        .map_err(|_| corrupt_pairing_state("active-binding"))?;
    Ok(Some(binding))
}

fn validate_active_binding(config: &ClientConfig, binding: &ActiveBinding) -> anyhow::Result<()> {
    validate_state_version(binding.version)?;
    if binding.generation.is_nil() || binding.request_id.is_nil() || binding.instance_id.is_nil() {
        bail!("active binding contains a nil UUID");
    }
    config
        .validate_durable_report_endpoint(&binding.report_endpoint)
        .context("active binding report endpoint is unsafe")
}

fn binding_from_active_state(state: &StoredPairingState) -> anyhow::Result<ActiveBinding> {
    let StoredPairingState::Active {
        version,
        generation,
        request_id,
        instance_id,
        report_endpoint,
        ..
    } = state
    else {
        bail!("internal error: expected an Active pairing state");
    };
    Ok(ActiveBinding {
        version: *version,
        generation: *generation,
        request_id: *request_id,
        instance_id: *instance_id,
        report_endpoint: report_endpoint.clone(),
    })
}

fn lock_state(config: &ClientConfig) -> anyhow::Result<StateTransaction> {
    StateTransaction::begin(&config.state_dir)
        .context("failed to open private credential state transaction")
}

fn load_state(store: &StateReader) -> anyhow::Result<Option<StoredPairingState>> {
    let path = store.path(StateFile::Pairing);
    let bytes = match store.read(StateFile::Pairing) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read pairing state {}", path.display()));
        }
    };
    let bytes = sarmg_client_secret::SecretBytes::new(bytes);
    let state: StoredPairingState = decode_pairing_document(bytes.expose(), "pairing-state")?;
    let (version, generation) = match &state {
        StoredPairingState::Creating {
            version,
            generation,
            ..
        }
        | StoredPairingState::Pending {
            version,
            generation,
            ..
        }
        | StoredPairingState::Activating {
            version,
            generation,
            ..
        }
        | StoredPairingState::Active {
            version,
            generation,
            ..
        }
        | StoredPairingState::Denied {
            version,
            generation,
            ..
        }
        | StoredPairingState::Expired {
            version,
            generation,
            ..
        } => (*version, *generation),
    };
    validate_state_version(version)?;
    if generation.is_nil() {
        return Err(corrupt_pairing_state("pairing-state"));
    }
    Ok(Some(state))
}

/// Archive only account/pairing artifacts. Host identity and telemetry spool are
/// deliberately outside this list and therefore survive re-pairing recovery.
pub fn archive_incompatible_account_state(config: &ClientConfig) -> anyhow::Result<Vec<PathBuf>> {
    let transaction = lock_state(config)?;
    let archive_id = Uuid::new_v4().simple().to_string();
    let mut archived = Vec::new();
    for file in [
        StateFile::Pairing,
        StateFile::Authorization,
        StateFile::Binding,
        StateFile::Credential,
    ] {
        let name = format!("{}.incompatible-{archive_id}", file.name());
        if let Some(path) = transaction
            .archive(file, &name)
            .with_context(|| format!("failed to archive incompatible {}", file.name()))?
        {
            archived.push(path);
        }
    }
    Ok(archived)
}

// These fragments intentionally remain in this module scope. Pairing commit
// and compare-and-swap helpers share private state-machine invariants; an
// `include!` split keeps those boundaries private while making the source
// navigable and keeping tests out of the production flow file.
include!("state_io.rs");
include!("tests.rs");
