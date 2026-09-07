/// Inspect the durable pairing journal without taking the transaction lock or
/// completing an interrupted current transaction.
///
/// `status` is a diagnostic command and must remain byte-for-byte read-only:
/// taking the normal lock can create the state directory/lock file, while the
/// recovery path can publish a credential and rewrite several state files.
/// Recovery remains the responsibility of `run` and `pair`.
pub fn local_progress(config: &ClientConfig) -> anyhow::Result<Option<PairingProgress>> {
    let reader = match StateReader::open(&config.state_dir) {
        Ok(reader) => reader,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    load_state(&reader).map(|state| state.map(progress_from_terminal))
}

/// Read pairing progress and its active endpoint binding as one stable, byte-for-byte read-only
/// snapshot. Re-reading the journal prevents an atomic replacement racing with the binding read
/// from manufacturing a false mismatch in diagnostics.
pub fn local_status(config: &ClientConfig) -> anyhow::Result<LocalPairingStatus> {
    const MAX_SNAPSHOT_ATTEMPTS: usize = 3;
    let reader = match StateReader::open(&config.state_dir) {
        Ok(reader) => reader,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LocalPairingStatus {
                progress: None,
                active_report_endpoint: None,
            });
        }
        Err(error) => return Err(error.into()),
    };
    let store = &reader;

    for _ in 0..MAX_SNAPSHOT_ATTEMPTS {
        let state = load_state(store)?;
        if !matches!(state.as_ref(), Some(StoredPairingState::Active { .. })) {
            return Ok(LocalPairingStatus {
                progress: state.map(progress_from_terminal),
                active_report_endpoint: None,
            });
        }
        let active = state.expect("checked Active pairing state above");
        let expected = binding_from_active_state(&active)?;
        let binding = load_active_binding(config, store);
        let current = load_state(store)?;
        let current_binding = match current.as_ref() {
            Some(current @ StoredPairingState::Active { .. }) => {
                Some(binding_from_active_state(current)?)
            }
            _ => None,
        };
        if current_binding.as_ref() != Some(&expected) {
            continue;
        }
        match binding? {
            Some(binding) if binding == expected => {}
            Some(_) => bail!("active binding does not match the current Active pairing state"),
            None => {
                bail!("active binding is missing; purge local state and pair host-monitor again")
            }
        }
        return Ok(LocalPairingStatus {
            progress: Some(progress_from_terminal(active)),
            active_report_endpoint: Some(expected.report_endpoint),
        });
    }
    bail!("pairing state changed repeatedly while reading local status")
}

/// Return whether the durable host-id belongs to a current package-version
/// pairing transaction that still has an authorized credential.
pub fn has_current_authorized_identity(config: &ClientConfig) -> anyhow::Result<bool> {
    let transaction = lock_state(config)?;
    let store = &transaction;
    let authorized = local_auth_state_unlocked(store)?
        .is_some_and(|state| state.status == CredentialAuthorization::Authorized);
    if !authorized {
        return Ok(false);
    }
    Ok(matches!(
        load_state(store)?,
        Some(
            StoredPairingState::Creating { .. }
                | StoredPairingState::Pending { .. }
                | StoredPairingState::Activating { .. }
                | StoredPairingState::Active { .. }
                | StoredPairingState::Denied { .. }
                | StoredPairingState::Expired { .. }
        )
    ))
}

fn config_for_active_binding(config: &ClientConfig, binding: &ActiveBinding) -> ClientConfig {
    let mut active = config.clone();
    apply_active_config(&mut active, &binding.report_endpoint);
    active
}

/// Product identity and endpoint accompany the same locked credential snapshot.
/// Applying a fully built snapshot cannot fail or partly mutate caller state.
pub struct ReporterSnapshot {
    reporter: Reporter,
    host: HostIdentity,
    report_endpoint: String,
}
impl ReporterSnapshot {
    pub fn credential_revision(&self) -> (Uuid, Uuid) {
        self.reporter.credential_revision()
    }

    pub fn apply(self, config: &mut ClientConfig, host: &mut HostIdentity) -> Reporter {
        apply_active_config(config, &self.report_endpoint);
        *host = self.host;
        self.reporter
    }
}

fn reporter_for_active_binding_unlocked(
    config: &ClientConfig,
    store: &StateTransaction,
    binding: &ActiveBinding,
) -> anyhow::Result<Option<ReporterSnapshot>> {
    let durable_host = crate::collectors::load_host_identity_from(store)?;
    crate::client_identity::for_instance(&durable_host.id)?.ensure_matches(
        &crate::client_identity::for_instance(&binding.instance_id.to_string())?
    )?;
    Ok(
        Reporter::for_existing_credential(&config_for_active_binding(config, binding), store)?.map(
            |reporter| ReporterSnapshot {
                reporter,
                host: durable_host,
                report_endpoint: binding.report_endpoint.clone(),
            },
        ),
    )
}

/// Re-read the currently authorized binding, independent of a potentially stale
/// network probe. An incomplete new pairing may coexist with a newer, usable
/// credential that this process has not observed yet.
pub fn refresh_reporter_snapshot(
    config: &ClientConfig,
    revision: (Uuid, Uuid),
) -> anyhow::Result<Option<ReporterSnapshot>> {
    let transaction = lock_state(config)?;
    if local_auth_state_unlocked(&transaction)?
        .is_none_or(|state| state.status != CredentialAuthorization::Authorized)
    {
        return Ok(None);
    }
    let Some(state) = load_state(&transaction)? else {
        return Ok(None);
    };
    if matches!(state, StoredPairingState::Activating { .. }) {
        // The next recovery poll completes the journal before exposing its files.
        return Ok(None);
    }
    let binding = load_active_binding(config, &transaction)?
        .context("authorized credential binding is missing")?;
    if matches!(state, StoredPairingState::Active { .. })
        && binding_from_active_state(&state)? != binding
    {
        bail!("active credential binding does not match the pairing journal");
    }
    if (binding.generation, binding.request_id) == revision {
        return Ok(None);
    }
    reporter_for_active_binding_unlocked(config, &transaction, &binding)
}

/// Return a consistent snapshot of the previously active reporter while a
/// new pairing attempt is incomplete. Reading the durable endpoint binding
/// and token under the same cross-process lock prevents observing a token with
/// an unrelated base configuration endpoint.
pub fn existing_reporter_for_run(config: &ClientConfig) -> anyhow::Result<Option<ReporterSnapshot>> {
    let transaction = lock_state(config)?;
    let store = &transaction;
    if local_auth_state_unlocked(store)?
        .is_none_or(|state| state.status != CredentialAuthorization::Authorized)
    {
        return Ok(None);
    }
    match load_state(store)? {
        Some(StoredPairingState::Active { .. }) => Ok(None),
        Some(activating @ StoredPairingState::Activating { .. }) => {
            finish_activating_unlocked(config, store, activating)?;
            Ok(None)
        }
        Some(
            StoredPairingState::Creating { .. }
            | StoredPairingState::Pending { .. }
            | StoredPairingState::Denied { .. }
            | StoredPairingState::Expired { .. },
        ) => match load_active_binding(config, store)? {
            Some(binding) => reporter_for_active_binding_unlocked(config, store, &binding),
            None => {
                bail!("active binding is missing; purge local state and pair host-monitor again")
            }
        },
        _ => Ok(None),
    }
}

/// Build a low-level transport only when every durable identity component is
/// bound to the current package's completed pairing transaction.
pub(crate) fn reporter_for_current_active_state(
    config: &ClientConfig,
) -> anyhow::Result<Option<Reporter>> {
    let transaction = lock_state(config)?;
    let store = &transaction;
    if local_auth_state_unlocked(store)?
        .is_none_or(|state| state.status != CredentialAuthorization::Authorized)
    {
        return Ok(None);
    }
    let Some(state @ StoredPairingState::Active { .. }) = load_state(store)? else {
        return Ok(None);
    };
    let expected = binding_from_active_state(&state)?;
    let binding = load_current_active_binding_unlocked(config, store, &expected)?;
    reporter_for_active_binding_unlocked(config, store, &binding)
        .map(|snapshot| snapshot.map(|snapshot| snapshot.reporter))
}

/// Revalidate the exact Active generation and durably converge the main
/// configuration before a caller starts using its token.
pub fn commit_active_configuration(
    config: &mut ClientConfig,
    generation: Uuid,
    request_id: Uuid,
    instance_id: Uuid,
    report_endpoint: &str,
) -> anyhow::Result<PathBuf> {
    let transaction = lock_state(config)?;
    let store = &transaction;
    let expected =
        ensure_active_is_current(store, generation, request_id, instance_id, report_endpoint)?;
    let binding = load_current_active_binding_unlocked(config, store, &expected)?;
    let path = persist_active_config_unlocked(config, &binding.report_endpoint)?;
    apply_active_config(config, &binding.report_endpoint);
    Ok(path)
}

/// Atomically snapshot the Active generation's config, identity and token into
/// an in-memory Reporter before allowing another pairing transaction to
/// replace them on disk.
pub fn activate_reporter_snapshot(
    config: &mut ClientConfig,
    host: &mut HostIdentity,
    generation: Uuid,
    request_id: Uuid,
    instance_id: Uuid,
    report_endpoint: &str,
) -> anyhow::Result<Reporter> {
    let transaction = lock_state(config)?;
    let store = &transaction;
    if local_auth_state_unlocked(store)?
        .is_none_or(|state| state.status != CredentialAuthorization::Authorized)
    {
        bail!("current Active pairing state has no current authorized identity state");
    }
    let expected =
        ensure_active_is_current(store, generation, request_id, instance_id, report_endpoint)?;
    let binding = load_current_active_binding_unlocked(config, store, &expected)?;
    let snapshot = reporter_for_active_binding_unlocked(config, store, &binding)?
        .context("paired host credential is missing after the Active pairing transaction")?;
    Ok(snapshot.apply(config, host))
}

fn ensure_active_is_current(
    store: &StateReader,
    generation: Uuid,
    request_id: Uuid,
    instance_id: Uuid,
    report_endpoint: &str,
) -> anyhow::Result<ActiveBinding> {
    let current = load_state(store)?;
    match current.as_ref() {
        Some(
            state @ StoredPairingState::Active {
                generation: current_generation,
                request_id: current_request_id,
                instance_id: current_instance_id,
                report_endpoint: current_report_endpoint,
                ..
            },
        ) if *current_generation == generation
            && *current_request_id == request_id
            && *current_instance_id == instance_id
            && current_report_endpoint == report_endpoint =>
        {
            binding_from_active_state(state)
        }
        _ => Err(PairingSuperseded.into()),
    }
}

/// Invalidate only the credential revision retained by the rejected Reporter.
pub fn mark_reauth_required_if_current(
    config: &ClientConfig,
    revision: (Uuid, Uuid),
    reason: impl Into<String>,
) -> anyhow::Result<bool> {
    let transaction = lock_state(config)?;
    let result =
        HostCredentials::new(config, &transaction).invalidate(&revision, &reason.into())?;
    Ok(result == sarmg_client_runtime::CredentialMutation::Applied)
}

pub fn local_auth_state(config: &ClientConfig) -> anyhow::Result<Option<LocalAuthState>> {
    match StateReader::open(&config.state_dir) {
        Ok(reader) => local_auth_state_unlocked(&reader),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn local_auth_state_unlocked(store: &StateReader) -> anyhow::Result<Option<LocalAuthState>> {
    let path = store.path(StateFile::Authorization);
    match store.read(StateFile::Authorization) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("auth state {} is invalid", path.display()))
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn persist_auth_state_unlocked(
    store: &StateTransaction,
    state: &LocalAuthState,
) -> anyhow::Result<()> {
    store
        .write(
            StateFile::Authorization,
            &serde_json::to_string_pretty(state)?,
        )
        .context("failed to persist authorization state")
}

fn progress_from_terminal(state: StoredPairingState) -> PairingProgress {
    match state {
        StoredPairingState::Creating {
            generation,
            report_endpoint,
            ..
        } => PairingProgress::Creating {
            generation,
            report_endpoint,
        },
        StoredPairingState::Pending {
            generation,
            request_id,
            activation_url,
            expires_at,
            poll_interval,
            ..
        } => PairingProgress::Waiting(PairingSession {
            generation,
            request_id,
            activation_url,
            expires_at,
            poll_interval,
        }),
        StoredPairingState::Activating {
            generation,
            report_endpoint,
            ..
        } => PairingProgress::Creating {
            generation,
            report_endpoint,
        },
        StoredPairingState::Active {
            generation,
            request_id,
            instance_id,
            report_endpoint,
            ..
        } => PairingProgress::Active {
            generation,
            request_id,
            instance_id,
            report_endpoint,
        },
        StoredPairingState::Denied {
            generation,
            request_id,
            activation_url,
            ..
        } => PairingProgress::Denied {
            generation,
            request_id,
            activation_url,
        },
        StoredPairingState::Expired {
            generation,
            request_id,
            activation_url,
            ..
        } => PairingProgress::Expired {
            generation,
            request_id,
            activation_url,
        },
    }
}
