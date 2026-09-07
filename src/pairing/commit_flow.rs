fn persist_active_credentials(
    config: &ClientConfig,
    pending: StoredPairingState,
    instance_id: Uuid,
) -> anyhow::Result<PairingProgress> {
    let StoredPairingState::Pending {
        generation,
        request_id,
        activation_url,
        expires_at,
        poll_interval,
        pairing_endpoint,
        report_endpoint,
        bearer_secret,
        polling_secret,
        ..
    } = pending
    else {
        bail!("internal error: expected pending pairing state for activation");
    };
    let transaction = lock_state(config)?;
    let store = &transaction;
    ensure_pending_is_current(
        store,
        generation,
        request_id,
        &pairing_endpoint,
        &report_endpoint,
        &polling_secret,
    )?;
    let activating = StoredPairingState::Activating {
        version: PAIRING_STATE_VERSION,
        generation,
        request_id,
        activation_url,
        expires_at,
        poll_interval,
        instance_id,
        pairing_endpoint,
        report_endpoint: report_endpoint.clone(),
        bearer_secret,
    };
    // Commit the journal before touching any long-lived credential. A crash
    // after this write is recovered locally and can never pair the new token
    // with the previous server endpoint.
    persist_state_unlocked(store, &activating)?;
    finish_activating_unlocked(config, store, activating)
}

fn session_from_activating(state: &StoredPairingState) -> anyhow::Result<PairingSession> {
    let StoredPairingState::Activating {
        generation,
        request_id,
        activation_url,
        expires_at,
        poll_interval,
        ..
    } = state
    else {
        bail!("internal error: expected an activating pairing state");
    };
    Ok(PairingSession {
        generation: *generation,
        request_id: *request_id,
        activation_url: activation_url.clone(),
        expires_at: *expires_at,
        poll_interval: *poll_interval,
    })
}

fn recover_activating(
    config: &ClientConfig,
    expected: StoredPairingState,
) -> anyhow::Result<PairingProgress> {
    let (expected_generation, expected_request_id) = match &expected {
        StoredPairingState::Activating {
            generation,
            request_id,
            ..
        } => (*generation, *request_id),
        _ => bail!("internal error: expected an activating pairing state"),
    };
    let transaction = lock_state(config)?;
    let store = &transaction;
    match load_state(store)? {
        Some(
            current @ StoredPairingState::Activating {
                generation,
                request_id,
                ..
            },
        ) if generation == expected_generation && request_id == expected_request_id => {
            finish_activating_unlocked(config, store, current)
        }
        Some(StoredPairingState::Active {
            generation,
            request_id,
            instance_id,
            report_endpoint,
            ..
        }) if generation == expected_generation && request_id == expected_request_id => {
            Ok(PairingProgress::Active {
                generation,
                request_id,
                instance_id,
                report_endpoint,
            })
        }
        _ => Err(PairingSuperseded.into()),
    }
}

fn finish_activating_unlocked(
    config: &ClientConfig,
    store: &StateTransaction,
    state: StoredPairingState,
) -> anyhow::Result<PairingProgress> {
    HostCredentials::new(config, store).replace(state)?;
    let active = load_state(store)?.context("credential rotation did not publish Active")?;
    Ok(progress_from_terminal(active))
}

/// Complete an Activating journal while the pairing state lock is held. Every
/// write is idempotent; Active is deliberately last so any earlier crash is
/// recoverable without consulting the remote server.
fn commit_activating_unlocked(
    config: &ClientConfig,
    store: &StateTransaction,
    state: StoredPairingState,
) -> anyhow::Result<PairingProgress> {
    let StoredPairingState::Activating {
        version,
        generation,
        request_id,
        activation_url,
        instance_id,
        report_endpoint,
        bearer_secret,
        ..
    } = state
    else {
        bail!("internal error: expected an activating pairing state");
    };
    validate_state_version(version)?;
    let binding = ActiveBinding {
        version: PAIRING_STATE_VERSION,
        generation,
        request_id,
        instance_id,
        report_endpoint: report_endpoint.clone(),
    };
    // Validate all durable identity components before replacing any credential.
    validate_active_binding(config, &binding)?;
    store.write(StateFile::Credential, bearer_secret.expose())?;
    store.write(StateFile::Identity, &instance_id.to_string())?;
    persist_active_binding_unlocked(config, store, &binding)?;
    persist_auth_state_unlocked(
        store,
        &LocalAuthState {
            version: PAIRING_STATE_VERSION,
            status: CredentialAuthorization::Authorized,
            reason: "browser pairing completed".into(),
            changed_at: Utc::now(),
        },
    )?;
    persist_state_unlocked(
        store,
        &StoredPairingState::Active {
            version: PAIRING_STATE_VERSION,
            generation,
            request_id,
            activation_url,
            instance_id,
            report_endpoint: report_endpoint.clone(),
            completed_at: Utc::now(),
        },
    )?;
    Ok(PairingProgress::Active {
        generation,
        request_id,
        instance_id,
        report_endpoint,
    })
}
