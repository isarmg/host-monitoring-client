use super::*;
use sarmg_client_runtime::CredentialMutation;

fn config() -> ClientConfig {
    ClientConfig {
        state_dir: std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-credential-store-{}", Uuid::new_v4())),
        ..ClientConfig::default()
    }
}

fn activating(config: &ClientConfig) -> StoredPairingState {
    StoredPairingState::Activating {
        version: PAIRING_STATE_VERSION,
        generation: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        activation_url: "https://host-monitoring.example/activate".into(),
        expires_at: Utc::now() + TimeDelta::minutes(10),
        poll_interval: 2,
        instance_id: Uuid::new_v4(),
        pairing_endpoint: config.pairing_endpoint(),
        report_endpoint: config.endpoint.clone(),
        bearer_secret: random_secret(),
    }
}

fn commit(config: &ClientConfig, state: StoredPairingState) -> Reporter {
    let transaction = lock_state(config).unwrap();
    persist_state_unlocked(&transaction, &state).unwrap();
    HostCredentials::new(config, &transaction)
        .replace(state)
        .unwrap();
    drop(transaction);
    Reporter::new(config).unwrap()
}

fn pending(config: &ClientConfig) -> StoredPairingState {
    StoredPairingState::Pending {
        version: PAIRING_STATE_VERSION,
        generation: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        activation_url: "https://host-monitoring.example/activate".into(),
        expires_at: Utc::now() + TimeDelta::minutes(10),
        poll_interval: 2,
        pairing_endpoint: config.pairing_endpoint(),
        report_endpoint: config.endpoint.clone(),
        bearer_secret: random_secret(),
        polling_secret: random_secret(),
    }
}

fn all_state_bytes(transaction: &StateTransaction) -> Vec<Vec<u8>> {
    [
        StateFile::Identity,
        StateFile::Credential,
        StateFile::Pairing,
        StateFile::Authorization,
        StateFile::Binding,
    ]
    .into_iter()
    .map(|file| transaction.read(file).unwrap())
    .collect()
}

#[test]
fn delayed_401_cannot_invalidate_a_new_credential_during_another_pending_pairing() {
    let config = config();
    let first = commit(&config, activating(&config));
    let second = commit(&config, activating(&config));
    let transaction = lock_state(&config).unwrap();
    persist_state_unlocked(&transaction, &pending(&config)).unwrap();
    let before = all_state_bytes(&transaction);
    drop(transaction);

    let resumed = existing_reporter_for_run(&config).unwrap().unwrap();
    assert_eq!(resumed.credential_revision(), second.credential_revision());
    assert!(
        !mark_reauth_required_if_current(&config, first.credential_revision(), "late 401").unwrap()
    );
    let transaction = lock_state(&config).unwrap();
    assert_eq!(all_state_bytes(&transaction), before);
    let snapshot = HostCredentials::new(&config, &transaction)
        .load()
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.revision, second.credential_revision());
    drop(transaction);

    assert!(
        mark_reauth_required_if_current(
            &config,
            resumed.credential_revision(),
            "HTTP 401 unauthorized"
        )
        .unwrap()
    );
    assert!(existing_reporter_for_run(&config).unwrap().is_none());
    let transaction = lock_state(&config).unwrap();
    assert!(
        HostCredentials::new(&config, &transaction)
            .load()
            .unwrap()
            .is_none()
    );
    assert_eq!(transaction.read(StateFile::Credential).unwrap(), before[1]);
    assert_eq!(transaction.read(StateFile::Pairing).unwrap(), before[2]);
    let auth = local_auth_state_unlocked(&transaction).unwrap().unwrap();
    assert_eq!(
        auth.status,
        CredentialAuthorization::ReauthorizationRequired
    );
    assert_eq!(auth.reason, "HTTP 401 unauthorized");
    drop(transaction);
    fs::remove_dir_all(config.state_dir).unwrap();
}

#[test]
fn rotation_rechecks_the_journal_and_late_invalidation_cannot_touch_its_commit() {
    let config = config();
    let first_journal = activating(&config);
    let first = commit(&config, first_journal.clone());
    let next = activating(&config);
    let transaction = lock_state(&config).unwrap();
    persist_state_unlocked(&transaction, &next).unwrap();
    let before = all_state_bytes(&transaction);
    let mut credentials = HostCredentials::new(&config, &transaction);
    assert_eq!(
        credentials
            .invalidate(&first.credential_revision(), "late 401")
            .unwrap(),
        CredentialMutation::Superseded
    );
    assert!(credentials.replace(first_journal).is_err());
    assert_eq!(all_state_bytes(&transaction), before);
    assert!(
        credentials.load().is_err(),
        "incomplete rotation cannot produce a snapshot"
    );
    credentials.replace(next).unwrap();
    let snapshot = credentials.load().unwrap().unwrap();
    assert_ne!(snapshot.revision, first.credential_revision());
    let committed = all_state_bytes(&transaction);
    assert_eq!(
        credentials
            .invalidate(&first.credential_revision(), "late 401")
            .unwrap(),
        CredentialMutation::Superseded
    );
    assert_eq!(all_state_bytes(&transaction), committed);
    assert_eq!(
        credentials
            .invalidate(&snapshot.revision, "current 401")
            .unwrap(),
        CredentialMutation::Applied
    );
    assert!(credentials.load().unwrap().is_none());
    assert_eq!(
        credentials
            .invalidate(&snapshot.revision, "current 401 repeated")
            .unwrap(),
        CredentialMutation::Applied
    );
    assert!(credentials.load().unwrap().is_none());
    assert_eq!(
        transaction.read(StateFile::Credential).unwrap(),
        committed[1]
    );
    drop(transaction);
    fs::remove_dir_all(config.state_dir).unwrap();
}

#[test]
fn credential_store_rejects_corrupt_bindings_instead_of_loading_raw_tokens() {
    let config = config();
    let transaction = lock_state(&config).unwrap();
    assert!(
        HostCredentials::new(&config, &transaction)
            .load()
            .unwrap()
            .is_none()
    );
    transaction
        .write(StateFile::Credential, "raw-unbound-token")
        .unwrap();
    assert!(
        HostCredentials::new(&config, &transaction)
            .load()
            .unwrap()
            .is_none()
    );
    drop(transaction);
    let reporter = commit(&config, activating(&config));
    let transaction = lock_state(&config).unwrap();
    let original = all_state_bytes(&transaction);
    transaction.write(StateFile::Binding, "invalid").unwrap();
    let mut credentials = HostCredentials::new(&config, &transaction);
    assert!(credentials.load().is_err());
    assert!(
        credentials
            .invalidate(&reporter.credential_revision(), "401")
            .is_err()
    );
    assert_eq!(
        transaction.read(StateFile::Authorization).unwrap(),
        original[3]
    );
    drop(transaction);
    fs::remove_dir_all(config.state_dir).unwrap();
}

#[test]
fn unknown_authorization_is_an_error_not_an_implicit_state_or_repair() {
    let config = config();
    let reporter = commit(&config, activating(&config));
    let transaction = lock_state(&config).unwrap();
    let invalid = serde_json::json!({
        "version": "0.9.4",
        "status": "private-invalid-authorization-marker",
        "reason": "fixture",
        "changed_at": Utc::now(),
    })
    .to_string();
    transaction
        .write(StateFile::Authorization, &invalid)
        .unwrap();
    let mut credentials = HostCredentials::new(&config, &transaction);
    let error = credentials.load().unwrap_err();
    assert!(!format!("{error:#}/{error:?}").contains("private-invalid-authorization-marker"));
    assert!(
        credentials
            .invalidate(&reporter.credential_revision(), "401")
            .is_err()
    );
    assert_eq!(
        transaction.read(StateFile::Authorization).unwrap(),
        invalid.as_bytes()
    );
    drop(transaction);
    fs::remove_dir_all(config.state_dir).unwrap();
}

#[test]
fn invalid_rotation_identity_is_rejected_before_any_credential_write() {
    let config = config();
    commit(&config, activating(&config));
    let transaction = lock_state(&config).unwrap();
    for invalid_request in [false, true] {
        let mut journal = activating(&config);
        if let StoredPairingState::Activating {
            instance_id,
            request_id,
            ..
        } = &mut journal
        {
            if invalid_request {
                *request_id = Uuid::nil();
            } else {
                *instance_id = Uuid::nil();
            }
        }
        persist_state_unlocked(&transaction, &journal).unwrap();
        let before = all_state_bytes(&transaction);
        assert!(
            HostCredentials::new(&config, &transaction)
                .replace(journal)
                .is_err()
        );
        assert_eq!(all_state_bytes(&transaction), before);
    }
    drop(transaction);
    fs::remove_dir_all(config.state_dir).unwrap();
}

#[test]
fn a_complete_snapshot_keeps_its_identity_and_endpoint_after_another_rotation() {
    let mut config = config();
    let mut next_config = config.clone();
    next_config.endpoint = "https://second.example/api/v2/host-monitor/report".into();
    let second = commit(&next_config, activating(&next_config));
    let transaction = lock_state(&config).unwrap();
    let binding = load_active_binding(&config, &transaction).unwrap().unwrap();
    persist_state_unlocked(&transaction, &pending(&config)).unwrap();
    drop(transaction);
    let snapshot = existing_reporter_for_run(&config).unwrap().unwrap();
    let third = commit(&config, activating(&config));
    assert_ne!(third.credential_revision(), second.credential_revision());
    let mut host = crate::collectors::load_host_identity(&config.state_dir).unwrap();
    let reporter = snapshot.apply(&mut config, &mut host);
    assert_eq!(reporter.credential_revision(), second.credential_revision());
    assert_eq!(host.id, binding.instance_id.to_string());
    assert_eq!(config.endpoint, binding.report_endpoint);
    assert!(
        refresh_reporter_snapshot(&config, reporter.credential_revision())
            .unwrap()
            .is_some()
    );
    fs::remove_dir_all(config.state_dir).unwrap();
}

#[test]
fn failed_active_snapshot_never_partly_updates_the_callers_identity_or_endpoint() {
    let mut config = config();
    commit(&config, activating(&config));
    let mut host = crate::collectors::load_host_identity(&config.state_dir).unwrap();
    let original_host = serde_json::to_value(&host).unwrap();
    let original_endpoint = config.endpoint.clone();
    config.pairing_endpoint = Some(config.pairing_endpoint());
    let original_pairing = config.pairing_endpoint.clone();
    let mut next_config = config.clone();
    next_config.endpoint = "https://second.example/api/v2/host-monitor/report".into();
    commit(&next_config, activating(&next_config));
    let transaction = lock_state(&config).unwrap();
    let binding = load_active_binding(&config, &transaction).unwrap().unwrap();
    // Keep metadata safe but inject an unreadable credential payload.
    fs::write(transaction.path(StateFile::Credential), []).unwrap();
    drop(transaction);
    assert!(
        activate_reporter_snapshot(
            &mut config,
            &mut host,
            binding.generation,
            binding.request_id,
            binding.instance_id,
            &binding.report_endpoint
        )
        .is_err()
    );
    assert_eq!(serde_json::to_value(&host).unwrap(), original_host);
    assert_eq!(config.endpoint, original_endpoint);
    assert_eq!(config.pairing_endpoint, original_pairing);
    fs::remove_dir_all(config.state_dir).unwrap();
}
