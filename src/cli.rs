use host_monitor::{
    ClientCommand, ClientConfig,
    maintenance::Guard,
    pairing::{self, PairingProgress},
};
use sarmg_client_cli::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn service() -> Service {
    Service {
        #[cfg(not(target_os = "macos"))]
        name: if cfg!(windows) {
            "host-monitor"
        } else {
            "host-monitor.service"
        },
        label: "org.sarmg.hostmonitor",
        default_config: host_monitor::config::default_config_path(),
        binary: "host-monitor",
        log_path: "/var/log/host-monitor.log",
    }
}
fn load(path: &Path) -> Result<ClientConfig> {
    let (mut c, _) = ClientConfig::load_selected_config(Some(path), ClientCommand::Probe)
        .map_err(state_read_error)?;
    c.config_path = Some(path.to_owned());
    Ok(c)
}

fn state_read_error(error: anyhow::Error) -> Failure {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    }) {
        return fail(4, "config_missing");
    }
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
    }) {
        return fail(3, "administrator_privileges_required");
    }
    if error
        .chain()
        .any(|cause| cause.downcast_ref::<std::io::Error>().is_some())
    {
        return fail(8, "state_read_failed");
    }
    storage_error(error)
}

fn config_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(fail(3, "administrator_privileges_required"))
        }
        Err(_) => Err(fail(8, "state_read_failed")),
    }
}
fn revision_of(c: &ClientConfig) -> Result<String> {
    Ok(revision(&serde_json::to_vec(c).map_err(storage_error)?))
}
fn queue(c: &ClientConfig) -> Result<Value> {
    let limits = sarmg_client_runtime::SpoolLimits {
        max_record_bytes: host_monitor::model::CLIENT_REPORT_MAX_BODY_BYTES,
        max_entries: sarmg_client_runtime::MAX_SPOOL_ENTRIES,
        max_bytes: c.spool_max_bytes,
    };
    match sarmg_client_runtime::Spool::inspect_existing(c.state_dir.join("spool"), limits) {
        Ok(h) => Ok(
            json!({"pending_batches":h.spool_entries,"bytes":h.spool_bytes,"quarantined":h.quarantined_entries,"identity_mismatch":h.identity_mismatch_entries,"healthy":h.healthy}),
        ),
        Err(sarmg_client_runtime::Error::Filesystem(sarmg_client_fs_safety::Error::Io(e)))
            if e.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(json!({"pending_batches":0,"bytes":0,"quarantined":0,"healthy":true}))
        }
        Err(e) => Err(storage_error(e)),
    }
}

fn ensure_private_directory(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "archive path is not a physical directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path)?;
        }
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn archive_queue(c: &ClientConfig, reason: &str) -> Result<Value> {
    if reason != "server-state-lost" {
        return Err(fail(2, "unsupported_archive_reason"));
    }
    let snapshot = queue(c)?;
    if snapshot["pending_batches"] == 0 && snapshot["quarantined"] == 0 {
        return Err(fail(4, "queue_empty"));
    }
    let identity = host_monitor::client_identity::load(&c.state_dir).map_err(storage_error)?;
    let spool = c.state_dir.join("spool");
    let spool_metadata = std::fs::symlink_metadata(&spool).map_err(storage_error)?;
    if spool_metadata.file_type().is_symlink() || !spool_metadata.is_dir() {
        return Err(fail(8, "unsafe_or_corrupt_state"));
    }
    let retired = c.state_dir.join("retired-bindings");
    ensure_private_directory(&retired).map_err(storage_error)?;
    let host = retired.join(identity.instance_id());
    ensure_private_directory(&host).map_err(storage_error)?;
    let archive_id = format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.6fZ"),
        uuid::Uuid::new_v4().simple()
    );
    let destination = host.join(archive_id);
    ensure_private_directory(&destination).map_err(storage_error)?;
    let archived_spool = destination.join("spool");
    std::fs::rename(&spool, &archived_spool).map_err(storage_error)?;
    let manifest = json!({
        "format": 1,
        "reason": reason,
        "host_id": identity.instance_id(),
        "archived_at": chrono::Utc::now(),
        "queue": snapshot,
    });
    let manifest_path = destination.join("manifest.json");
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&manifest_path)?;
        serde_json::to_writer_pretty(&mut file, &manifest).map_err(std::io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()
    })();
    if let Err(error) = result {
        let _ = std::fs::rename(&archived_spool, &spool);
        let _ = std::fs::remove_dir(&destination);
        return Err(storage_error(error));
    }
    Ok(json!({
        "archived": true,
        "host_id": identity.instance_id(),
        "archive": destination,
        "queue": snapshot,
    }))
}
fn local_runtime_status(c: &ClientConfig) -> Result<Value> {
    let mut value =
        crate::monitor_app::diagnostics::local_status_snapshot(c).map_err(storage_error)?;
    let id = value["host_id"].as_str();
    let runtime = host_monitor::runtime_status::read(&c.state_dir, id);
    let effective = runtime.as_ref().map(|s| s["effective_revision"].clone());
    value["config_path"] = value["config"].clone();
    value["config"] = json!({"stored_revision":revision_of(c)?,"effective_revision":effective,"restart_required":effective.as_ref().map(|r|r!=&json!(revision_of(c).unwrap_or_default()))});
    value["runtime"] =
        runtime.unwrap_or(json!({"available":false,"reason":"runtime_summary_unavailable"}));
    let recent_ack = value["runtime"]["last_ack_at"].as_i64().is_some_and(|at| {
        let age = chrono::Utc::now().timestamp() - at;
        age >= 0 && (age as u64) <= c.interval_seconds.saturating_mul(3) + c.request_timeout_seconds
    });
    let healthy = recent_ack
        && value["config"]["restart_required"] == false
        && value["spool_invalid_batches"] == 0
        && value["status"] == "configured";
    value["health"] = json!(if healthy { "healthy" } else { "unknown" });
    Ok(value)
}

fn local_status(c: &ClientConfig) -> Result<Value> {
    let mut value = local_runtime_status(c)?;
    value["service"] = service()
        .status(std::time::Duration::from_secs(5))
        .unwrap_or(json!({"state":"unknown"}));
    Ok(value)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairInput {
    server: String,
    authorization_code: String,
}
impl Drop for PairInput {
    fn drop(&mut self) {
        // Erase input allocation after the core has consumed it.
        let mut s = std::mem::take(&mut self.authorization_code).into_bytes();
        s.fill(0);
    }
}
fn pairing_failure(error: anyhow::Error, fallback: u8, code: &'static str) -> Failure {
    if let Some(http) = error.downcast_ref::<pairing::PairingHttpError>() {
        if http.code == Some("unsupported_client_protocol") {
            let mut failure = fail(10, "pairing_protocol_unsupported");
            failure.detail = Some(format!(
                "client_protocol={:?};server_supported={:?}",
                http.received, http.supported
            ));
            return failure;
        }
        return match http.status {
            401 | 403 => fail(7, "pairing_authorization_rejected"),
            408 | 429 | 500..=599 => fail(6, "pairing_server_unavailable"),
            404 | 405 | 406 | 426 => fail(10, "pairing_protocol_unsupported"),
            400..=499 => fail(2, "pairing_request_rejected"),
            _ => fail(10, "pairing_protocol_unsupported"),
        };
    }
    if let Some(io) = error.downcast_ref::<std::io::Error>()
        && io.kind() == std::io::ErrorKind::PermissionDenied
    {
        return fail(3, "permission_denied");
    }
    fail(fallback, code)
}
async fn pair(args: &Args, mut c: ClientConfig) -> Result<Value> {
    c.validate(ClientCommand::Pair)
        .map_err(|_| fail(2, "invalid_configuration"))?;
    let resume = args.words.get(1).is_some_and(|s| s == "resume");
    let replace = args.words.get(1).is_some_and(|s| s == "replace");
    let recover = args.words.get(1).is_some_and(|s| s == "recover");
    if resume && (args.has("--input-stdin") || args.has("--interactive") || args.has("--server")) {
        return Err(fail(2, "resume_uses_existing_transaction"));
    }
    let input = if resume {
        None
    } else if args.has("--input-stdin") {
        Some(stdin_document::<PairInput>(args.timeout)?)
    } else if args.has("--interactive") {
        Some(PairInput {
            server: if let Some(server) = args.get("--server") {
                server.into()
            } else {
                prompt("Server origin", false)?
            },
            authorization_code: prompt("Authorization code", true)?,
        })
    } else {
        return Err(fail(2, "protected_input_required"));
    };
    if let Some(i) = &input {
        if args.get("--server").is_some_and(|s| s != i.server) {
            return Err(fail(2, "server_input_mismatch"));
        }
        let server =
            host_monitor::pairing_input::validate_server_base(&i.server).map_err(input_error)?;
        host_monitor::pairing_input::validate_activation_code(&i.authorization_code)
            .map_err(input_error)?;
        c.endpoint = format!("{server}{}", host_protocol::CLIENT_REPORT_PATH);
        c.pairing_endpoint = Some(format!(
            "{server}{}",
            host_protocol::CLIENT_PAIRING_REQUESTS_PATH
        ));
    }
    let _guard = Guard::acquire(&c.state_dir).map_err(runtime_error)?;
    let existing = pairing::local_status(&c).map_err(storage_error)?;
    if resume && existing.progress.is_none() {
        return Err(fail(4, "no_pairing_transaction"));
    }
    if existing.active_report_endpoint.is_some() && !resume && !replace && !recover {
        return Err(fail(5, "binding_already_active_use_pair_replace"));
    }
    if replace {
        if !args.has("--confirm-replace")
            || args.require("--expected-binding")?
                != host_monitor::client_identity::load(&c.state_dir)
                    .map_err(storage_error)?
                    .instance_id()
        {
            return Err(fail(2, "binding_confirmation_required"));
        }
        let q = queue(&c)?;
        if q["pending_batches"] != 0 || q["quarantined"] != 0 {
            return Err(fail(5, "old_binding_queue_not_empty"));
        }
        c.replace_pending_pairing = true;
    }
    if !resume && !recover {
        let pending = queue(&c)?;
        if pending["pending_batches"] != 0 || pending["quarantined"] != 0 {
            return Err(fail(5, "old_binding_queue_not_empty"));
        }
    }
    let operation = async {
        if !resume {
            let (host, mode) = if recover {
                (
                    host_monitor::collectors::load_host_identity(&c.state_dir)
                        .map_err(storage_error)?,
                    pairing::PairMode::RecoverIdentity,
                )
            } else {
                (
                    host_monitor::collectors::transient_host_identity(uuid::Uuid::new_v4()),
                    pairing::PairMode::Fresh,
                )
            };
            if recover {
                c.replace_pending_pairing = true;
            }
            let session = pairing::start_or_resume_with_mode(&c, &host, mode)
                .await
                .map_err(|e| pairing_failure(e, 6, "pairing_create_unconfirmed"))?;
            if let Some(i) = &input {
                pairing::activate_pending_with_code(
                    &c,
                    session.generation,
                    session.request_id,
                    &i.authorization_code,
                )
                .await
                .map_err(|e| pairing_failure(e, 9, "activation_result_unconfirmed"))?;
            }
        }
        loop {
            let progress = pairing::poll_existing(&c)
                .await
                .map_err(|e| pairing_failure(e, 6, "pairing_query_unavailable"))?
                .ok_or_else(|| fail(8, "pairing_transaction_missing"))?;
            match progress {
                PairingProgress::Active {
                    generation,
                    request_id,
                    instance_id,
                    report_endpoint,
                } => {
                    pairing::commit_active_configuration(
                        &mut c,
                        generation,
                        request_id,
                        instance_id,
                        &report_endpoint,
                    )
                    .map_err(|_| Failure {
                        exit: 11,
                        code: "binding_committed_configuration_unconfirmed",
                        committed: true,
                        transaction_id: Some(request_id.to_string()),
                        step: None,
                        detail: None,
                    })?;
                    return Ok(
                        json!({"committed":true,"generation":generation,"transaction_id":request_id,"instance_id":instance_id,"endpoint":report_endpoint}),
                    );
                }
                PairingProgress::Waiting(session) => {
                    tokio::time::sleep(std::time::Duration::from_secs(session.poll_interval)).await
                }
                PairingProgress::Creating { .. } => {}
                PairingProgress::Denied { .. } => return Err(fail(7, "pairing_rejected")),
                PairingProgress::Expired { .. } => return Err(fail(7, "pairing_expired")),
            }
        }
    };
    let mut result = tokio::select! {r=tokio::time::timeout(args.timeout,operation)=>r.unwrap_or_else(|_|Err(fail(9,"pairing_result_unconfirmed"))),_=tokio::signal::ctrl_c()=>Err(fail(130,"interrupted_resume_required"))};
    if let Err(error) = &mut result
        && error.transaction_id.is_none()
        && let Ok(saved) = pairing::local_status(&c)
        && let Some(progress) = saved.progress
    {
        error.transaction_id = Some(
            match progress {
                PairingProgress::Creating { generation, .. } => generation,
                PairingProgress::Waiting(session) => session.request_id,
                PairingProgress::Active { request_id, .. }
                | PairingProgress::Denied { request_id, .. }
                | PairingProgress::Expired { request_id, .. } => request_id,
            }
            .to_string(),
        );
    }
    result
}

fn ask_yes_no(label: &str, default: bool) -> Result<bool> {
    let value = prompt(
        &format!("{label} [{}]", if default { "yes" } else { "no" }),
        false,
    )?;
    match value.trim().to_ascii_lowercase().as_str() {
        "" => Ok(default),
        "y" | "yes" | "true" | "1" => Ok(true),
        "n" | "no" | "false" | "0" => Ok(false),
        _ => Err(fail(2, "invalid_confirmation")),
    }
}

fn setup_args(args: &Args, words: Vec<String>, interactive: bool) -> Args {
    let mut options: std::collections::BTreeMap<String, String> = args
        .options
        .iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "--format"
                    | "--timeout"
                    | "--config"
                    | "--non-interactive"
                    | "--no-color"
                    | "--input-stdin"
                    | "--server"
            )
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    if interactive {
        options.insert("--interactive".into(), "true".into());
    }
    Args {
        words,
        options,
        format: args.format.clone(),
        timeout: args.timeout,
    }
}

fn setup_service_args(args: &Args, timeout: Duration) -> Args {
    let options = args
        .options
        .iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "--format" | "--timeout" | "--config" | "--non-interactive" | "--no-color"
            )
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    Args {
        words: vec![],
        options,
        format: args.format.clone(),
        timeout,
    }
}

#[derive(Clone, Copy)]
struct SetupDeadline {
    expires_at: Instant,
}

impl SetupDeadline {
    fn new(timeout: Duration) -> Self {
        Self {
            expires_at: Instant::now() + timeout,
        }
    }

    fn remaining(self, step: &'static str) -> Result<Duration> {
        self.expires_at
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| fail(9, "setup_deadline_exceeded").at_step(step))
    }
}

fn preserve_setup_commit(mut error: Failure, pairing: &Value) -> Failure {
    error.committed = true;
    if error.transaction_id.is_none() {
        error.transaction_id = pairing["transaction_id"].as_str().map(str::to_owned);
    }
    error
}

fn setup_failure(error: Failure, pairing: &Value, step: &'static str) -> Failure {
    preserve_setup_commit(error, pairing).at_step(step)
}

fn startup_policy_matches(status: &Value, enabled: bool) -> bool {
    let observed = status["startup"].as_str().unwrap_or("unknown");
    if enabled {
        matches!(observed, "automatic" | "enabled" | "enabled-runtime")
    } else {
        matches!(observed, "manual" | "disabled")
    }
}

#[derive(Debug)]
enum ExistingBindingState {
    None,
    LocalOnly,
    RemoteVerified {
        host_id: String,
    },
    Unauthorized,
    RemoteFailure(host_monitor::transport::RemoteBindingFailure),
    ServerChanged {
        configured: String,
        requested: String,
    },
    ConcurrentlyChanged,
    ProtocolUnsupported {
        received: u16,
        supported: Vec<u16>,
    },
}

fn requested_server_change(
    existing: &pairing::LocalPairingStatus,
    requested_endpoint: Option<&str>,
) -> Option<ExistingBindingState> {
    match (&existing.active_report_endpoint, requested_endpoint) {
        (Some(configured), Some(requested)) if configured != requested => {
            Some(ExistingBindingState::ServerChanged {
                configured: configured.clone(),
                requested: requested.to_owned(),
            })
        }
        _ => None,
    }
}

async fn existing_binding_state(
    c: &ClientConfig,
    existing: &pairing::LocalPairingStatus,
    deadline: SetupDeadline,
) -> ExistingBindingState {
    if existing.active_report_endpoint.is_none() {
        return ExistingBindingState::None;
    }
    for _ in 0..3 {
        let timeout = match deadline.remaining("pairing") {
            Ok(timeout) => timeout,
            Err(_) => {
                return ExistingBindingState::RemoteFailure(
                    host_monitor::transport::RemoteBindingFailure::Timeout,
                );
            }
        };
        let reporter = match host_monitor::transport::Reporter::new_with_timeout(c, timeout) {
            Ok(reporter) => reporter,
            Err(error)
                if error.chain().any(|cause| {
                    cause
                        .downcast_ref::<host_monitor::transport::LocalCredentialCorrupt>()
                        .is_some()
                }) =>
            {
                return ExistingBindingState::RemoteFailure(
                    host_monitor::transport::RemoteBindingFailure::LocalCredentialCorrupt,
                );
            }
            Err(_) => return ExistingBindingState::LocalOnly,
        };
        let revision = reporter.credential_revision();
        let result = reporter.verify_remote_binding().await;
        match pairing::active_credential_revision(c) {
            Ok(Some(current)) if current != revision => continue,
            Ok(Some(_)) => {
                return match result {
                    Ok(host_monitor::transport::RemoteBindingStatus::Authorized { host_id }) => {
                        ExistingBindingState::RemoteVerified { host_id }
                    }
                    Ok(host_monitor::transport::RemoteBindingStatus::Unauthorized) => {
                        ExistingBindingState::Unauthorized
                    }
                    Ok(host_monitor::transport::RemoteBindingStatus::ProtocolUnsupported {
                        received,
                        supported,
                    }) => ExistingBindingState::ProtocolUnsupported {
                        received,
                        supported,
                    },
                    Err(error) => ExistingBindingState::RemoteFailure(error),
                };
            }
            Ok(None) => return ExistingBindingState::ConcurrentlyChanged,
            Err(_) => return ExistingBindingState::LocalOnly,
        }
    }
    ExistingBindingState::ConcurrentlyChanged
}

fn record_setup_step(
    steps: &mut Vec<Value>,
    interactive: bool,
    step: &'static str,
    status: &'static str,
    evidence: Value,
) {
    if interactive {
        eprintln!("[setup] {step}: {status}");
    }
    steps.push(json!({"step":step,"status":status,"evidence":evidence}));
}

async fn wait_for_healthy(
    c: &ClientConfig,
    timeout: std::time::Duration,
    interactive: bool,
) -> Result<Value> {
    let started = tokio::time::Instant::now();
    let total = timeout.as_secs();
    let config = c.clone();
    let operation = async move {
        let mut last_progress = 0;
        loop {
            let snapshot_config = config.clone();
            let status =
                tokio::task::spawn_blocking(move || local_runtime_status(&snapshot_config))
                    .await
                    .map_err(storage_error)??;
            if status["health"] == "healthy" {
                return Ok(status);
            }
            let elapsed = started.elapsed().as_secs();
            if interactive && elapsed >= last_progress + 5 {
                last_progress = elapsed;
                eprintln!(
                    "[setup] connection: waiting ({}/{total} seconds)",
                    elapsed.min(total)
                );
                if let Some(code) = status["runtime"]["last_error_code"].as_str() {
                    eprintln!("[setup] connection: last_failure={code}");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    };
    tokio::select! {
        result = tokio::time::timeout(timeout, operation) => match result {
            Ok(result) => result,
            Err(_) => {
                let mut error = fail(9, "connection_unconfirmed");
                if let Ok(identity) = host_monitor::client_identity::load(&c.state_dir)
                    && let Some(runtime) = host_monitor::runtime_status::read(
                        &c.state_dir,
                        Some(identity.instance_id()),
                    )
                {
                    let code = runtime["last_error_code"].as_str().unwrap_or("no_acknowledgement");
                    let status = runtime["last_http_status"].as_u64();
                    error.detail = Some(format!("last_error_code={code};last_http_status={status:?}"));
                }
                Err(error)
            }
        },
        _ = tokio::signal::ctrl_c() => {
            let mut error = fail(130, "interrupted_connection_wait");
            error.detail = Some("pairing remains committed; rerun status --check to verify later".into());
            Err(error)
        }
    }
}

async fn setup(args: &Args, path: PathBuf, c: ClientConfig) -> Result<Value> {
    let deadline = SetupDeadline::new(args.timeout);
    if path != service().default_config {
        return Err(fail(2, "service_config_mismatch").at_step("configuration"));
    }
    let interactive = !args.has("--non-interactive") && !args.has("--input-stdin");
    let mut steps = Vec::new();
    c.validate(ClientCommand::Pair)
        .map_err(|_| fail(2, "invalid_configuration").at_step("configuration"))?;
    let requested_server = args
        .get("--server")
        .map(host_monitor::pairing_input::validate_server_base)
        .transpose()
        .map_err(|_| fail(2, "invalid_server_origin").at_step("configuration"))?;
    let service_api = service();
    let initial_service = service_api
        .status(deadline.remaining("service_inspection")?)
        .map_err(|error| error.at_step("service_inspection"))?;
    let (default_enable, default_start) = setup_service_intent(&initial_service);
    let existing =
        pairing::local_status(&c).map_err(|error| storage_error(error).at_step("configuration"))?;
    let reauthorization_required = pairing::local_auth_state(&c)
        .map_err(|error| storage_error(error).at_step("configuration"))?
        .is_some_and(|state| {
            state.status == sarmg_client_runtime::CredentialAuthorization::ReauthorizationRequired
        });
    if existing.active_report_endpoint.is_some() && interactive {
        eprintln!("[setup] pairing: local_binding_found");
    }
    let requested_endpoint = requested_server
        .as_ref()
        .map(|server| format!("{server}{}", host_protocol::CLIENT_REPORT_PATH));
    let service_verified_host = if initial_service["state"] == "running" {
        local_runtime_status(&c).ok().and_then(|status| {
            (status["health"] == "healthy")
                .then(|| status["host_id"].as_str().map(str::to_owned))
                .flatten()
        })
    } else {
        None
    };
    let mut binding_state = match requested_server_change(&existing, requested_endpoint.as_deref())
    {
        Some(change) => change,
        _ if service_verified_host.is_some() => ExistingBindingState::RemoteVerified {
            host_id: service_verified_host.expect("checked above"),
        },
        _ => existing_binding_state(&c, &existing, deadline).await,
    };
    if reauthorization_required
        && matches!(
            binding_state,
            ExistingBindingState::RemoteFailure(
                host_monitor::transport::RemoteBindingFailure::ServerUnavailable { .. }
            )
        )
    {
        binding_state = ExistingBindingState::Unauthorized;
    }
    let mut recover_changed_server = false;
    let reuse_pairing = match &binding_state {
        ExistingBindingState::RemoteVerified { host_id } => {
            if interactive {
                eprintln!("[setup] pairing: remotely_verified ({host_id})");
                ask_yes_no("Reuse this remotely verified binding?", true)
                    .map_err(|error| error.at_step("pairing"))?
            } else {
                true
            }
        }
        ExistingBindingState::None => false,
        ExistingBindingState::Unauthorized => {
            if interactive {
                eprintln!("[setup] pairing: stale_binding");
            }
            false
        }
        ExistingBindingState::LocalOnly => {
            return Err(fail(8, "local_binding_incomplete").at_step("pairing"));
        }
        ExistingBindingState::RemoteFailure(failure) => {
            let mut error = fail(6, failure.stable_code()).at_step("pairing");
            error.detail = match failure {
                host_monitor::transport::RemoteBindingFailure::ServerUnavailable { status }
                | host_monitor::transport::RemoteBindingFailure::UnexpectedRedirect { status }
                | host_monitor::transport::RemoteBindingFailure::ContractMismatch { status } => {
                    Some(format!("http_status={status}"))
                }
                _ => None,
            };
            return Err(error);
        }
        ExistingBindingState::ServerChanged {
            configured,
            requested,
        } => {
            if interactive {
                eprintln!("[setup] configured_server: {configured}");
                eprintln!("[setup] requested_server: {requested}");
                recover_changed_server = ask_yes_no(
                    "Server changed. Recover the existing Host identity on the requested Server?",
                    false,
                )
                .map_err(|error| error.at_step("pairing"))?;
            } else {
                recover_changed_server = args.has("--input-stdin");
            }
            if !recover_changed_server {
                return Err(fail(5, "server_change_confirmation_required").at_step("pairing"));
            }
            false
        }
        ExistingBindingState::ConcurrentlyChanged => {
            return Err(fail(6, "pairing_binding_changed_during_check").at_step("pairing"));
        }
        ExistingBindingState::ProtocolUnsupported {
            received,
            supported,
        } => {
            let mut error = fail(10, "pairing_protocol_unsupported").at_step("pairing");
            error.detail = Some(format!(
                "client_protocol={received};server_supported={supported:?}"
            ));
            return Err(error);
        }
    };
    if reuse_pairing {
        if args.has("--input-stdin") {
            return Err(
                fail(5, "active_setup_input_requires_pair_replace").at_step("configuration")
            );
        }
        if let Some(server) = args.get("--server") {
            let server = host_monitor::pairing_input::validate_server_base(server)
                .map_err(|_| fail(2, "invalid_server_origin").at_step("configuration"))?;
            let expected = format!("{server}{}", host_protocol::CLIENT_REPORT_PATH);
            if c.endpoint != expected {
                return Err(
                    fail(5, "server_replacement_requires_pair_replace").at_step("configuration")
                );
            }
        }
    }
    record_setup_step(
        &mut steps,
        interactive,
        "configuration",
        "verified",
        json!({"path":path,"service_path_matches":true}),
    );
    let pairing = if reuse_pairing {
        json!({"committed":true,"already_active":true})
    } else {
        let service_status = service_api
            .status(deadline.remaining("service_quiesce")?)
            .map_err(|error| error.at_step("service_quiesce"))?;
        if service_status["state"] == "running" {
            let stopped = service_api
                .change(
                    &setup_service_args(args, deadline.remaining("service_quiesce")?),
                    "stop",
                    &path,
                )
                .map_err(|error| error.at_step("service_quiesce"))?;
            if stopped["state"] == "running" {
                return Err(fail(11, "service_state_unconfirmed").at_step("service_quiesce"));
            }
        }
        let has_protected_input = args.has("--input-stdin") || args.has("--interactive");
        let resume = existing.progress.is_some()
            && !has_protected_input
            && args.get("--server").is_none()
            && !args.has("--non-interactive");
        let pair_words = if matches!(&binding_state, ExistingBindingState::Unauthorized)
            || recover_changed_server
        {
            vec!["pair".into(), "recover".into()]
        } else if matches!(&binding_state, ExistingBindingState::RemoteVerified { .. }) {
            vec!["pair".into(), "replace".into()]
        } else if resume {
            vec!["pair".into(), "resume".into()]
        } else {
            vec!["pair".into()]
        };
        let interactive = !resume && !args.has("--input-stdin") && !args.has("--non-interactive");
        let mut pair_args = setup_args(args, pair_words, interactive);
        pair_args.timeout = deadline.remaining("pairing")?;
        if matches!(&binding_state, ExistingBindingState::RemoteVerified { .. }) {
            let binding = host_monitor::client_identity::load(&c.state_dir)
                .map_err(|error| storage_error(error).at_step("pairing"))?;
            pair_args
                .options
                .insert("--confirm-replace".into(), "true".into());
            pair_args.options.insert(
                "--expected-binding".into(),
                binding.instance_id().to_owned(),
            );
        }
        pair(&pair_args, c)
            .await
            .map_err(|error| error.at_step("pairing"))?
    };
    let config = match load(&path) {
        Ok(config) => config,
        Err(error) => return Err(setup_failure(error, &pairing, "pairing")),
    };
    let pairing_status = pairing::local_status(&config)
        .map_err(|error| setup_failure(storage_error(error), &pairing, "pairing"))?;
    if pairing_status.active_report_endpoint.is_none() {
        return Err(setup_failure(
            fail(11, "pairing_postcondition_unconfirmed"),
            &pairing,
            "pairing",
        ));
    }
    record_setup_step(
        &mut steps,
        interactive,
        "pairing",
        "verified",
        json!({"state":"active","durable_identity":true}),
    );
    let (enable, start, verify) = if interactive {
        (
            ask_yes_no("Enable service at system startup?", default_enable)
                .map_err(|error| setup_failure(error, &pairing, "preferences"))?,
            ask_yes_no("Run the service now?", default_start)
                .map_err(|error| setup_failure(error, &pairing, "preferences"))?,
            ask_yes_no("Verify the connection now?", default_start)
                .map_err(|error| setup_failure(error, &pairing, "preferences"))?,
        )
    } else {
        (default_enable, default_start, default_start)
    };
    let registered = service_api
        .verified_status(deadline.remaining("service_registration")?, &path)
        .map_err(|error| setup_failure(error, &pairing, "service_registration"))?;
    record_setup_step(
        &mut steps,
        interactive,
        "service_registration",
        "verified",
        json!({"installed":registered["installed"],"registration_matches":true}),
    );
    let policy_action = if enable { "enable" } else { "disable" };
    let policy_status = service_api
        .change(
            &setup_service_args(args, deadline.remaining("startup_policy")?),
            policy_action,
            &path,
        )
        .map_err(|error| setup_failure(error, &pairing, "startup_policy"))?;
    if !startup_policy_matches(&policy_status, enable) {
        return Err(setup_failure(
            fail(11, "startup_policy_unconfirmed"),
            &pairing,
            "startup_policy",
        ));
    }
    record_setup_step(
        &mut steps,
        interactive,
        "startup_policy",
        "verified",
        json!({"requested":if enable {"enabled"} else {"disabled"},"observed":policy_status["startup"]}),
    );
    let service_result = if start {
        let status = service_api
            .change(
                &setup_service_args(args, deadline.remaining("service_runtime")?),
                "start",
                &path,
            )
            .map_err(|error| setup_failure(error, &pairing, "service_runtime"))?;
        if status["state"] != "running" {
            return Err(setup_failure(
                fail(11, "service_state_unconfirmed"),
                &pairing,
                "service_runtime",
            ));
        }
        record_setup_step(
            &mut steps,
            interactive,
            "service_runtime",
            "verified",
            json!({"requested":"running","observed":status["state"]}),
        );
        status
    } else {
        let current = service_api
            .verified_status(deadline.remaining("service_runtime")?, &path)
            .map_err(|error| setup_failure(error, &pairing, "service_runtime"))?;
        let status = if current["state"] == "running" {
            service_api
                .change(
                    &setup_service_args(args, deadline.remaining("service_runtime")?),
                    "stop",
                    &path,
                )
                .map_err(|error| setup_failure(error, &pairing, "service_runtime"))?
        } else {
            current
        };
        record_setup_step(
            &mut steps,
            interactive,
            "service_runtime",
            "skipped",
            json!({"reason":"not_requested","observed":status["state"]}),
        );
        status
    };
    let verification = if verify {
        if service_result["state"] != "running" {
            return Err(setup_failure(
                fail(4, "verification_requires_running_service"),
                &pairing,
                "connection",
            ));
        }
        match wait_for_healthy(&config, deadline.remaining("connection")?, interactive).await {
            Ok(status) => {
                record_setup_step(
                    &mut steps,
                    interactive,
                    "connection",
                    "verified",
                    json!({"health":status["health"]}),
                );
                status
            }
            Err(error) => return Err(setup_failure(error, &pairing, "connection")),
        }
    } else {
        record_setup_step(
            &mut steps,
            interactive,
            "connection",
            "skipped",
            json!({"reason":"not_requested"}),
        );
        json!({"state":"skipped","reason":"not_requested"})
    };
    Ok(json!({
        "setup":"completed",
        "steps":steps,
        "pairing":pairing,
        "service":service_result,
        "verification":verification
    }))
}

pub fn entry(raw: Vec<String>) -> u8 {
    let parse_format = requested_error_format(&raw);
    #[cfg(windows)]
    let elevation_raw = raw.clone();
    let args = match Args::parse(
        raw,
        &[
            "--file",
            "--server",
            "--reason",
            "--expected-revision",
            "--expected-binding",
        ],
        &["--network", "--delivery", "--confirm-replace"],
    ) {
        Ok(a) => a,
        Err(e) => return emit("host-monitor", "parse", parse_format, &Err(e)),
    };
    // Informational commands must never trigger UAC, even if setup appears in
    // the remaining words of a malformed invocation.
    if args.has("--help") {
        println!(
            "host-monitor: setup; config init|show|edit|validate|diff|apply; pair [status|resume|replace|recover]; queue status|inspect|drain|archive; status; doctor; service status|start|stop|restart|enable|disable; run; once; probe; version\nGlobal: --config ABSOLUTE_PATH --format human|json|ndjson --non-interactive --timeout 60s --no-color\nsetup/pair uses --interactive or --input-stdin JSON containing server and authorization_code. `pair recover` preserves the current Host UUID and queued reports. `queue archive --reason server-state-lost` atomically retires an old binding queue. Never pass secrets as arguments.\nconfig edit uses VISUAL or EDITOR and commits through the same revision check as config apply. Stop the service before writes."
        );
        return 0;
    }
    if (args.has("--version") || args.words == ["version"]) && args.format == "human" {
        println!("host-monitor {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    #[cfg(windows)]
    if args.words == ["setup"] {
        let interactive = !args.has("--non-interactive") && !args.has("--input-stdin");
        let installer_session = args.has("--installer-session");
        match prepare_windows_setup_elevation(
            &elevation_raw,
            interactive,
            installer_session,
            args.has("--elevated-setup-child"),
        ) {
            Ok(WindowsSetupElevation::Continue) => {}
            Ok(WindowsSetupElevation::ChildExited(exit)) => {
                if !installer_session {
                    if exit == 0 {
                        println!("Setup completed with administrator privileges.");
                    } else {
                        eprintln!(
                            "Setup failed with exit code {exit}. Run `host-monitor setup` from an Administrator terminal to keep the error visible."
                        );
                    }
                }
                return exit;
            }
            Err(error) => return emit("host-monitor", "setup", &args.format, &Err(error)),
        }
    }
    if args.words.is_empty() && !args.has("--version") {
        return no_args(&args);
    }
    if args.has("--follow") {
        if args.words != ["logs"] || args.format != "ndjson" {
            return emit(
                "host-monitor",
                "logs",
                &args.format,
                &Err(fail(2, "follow_requires_logs_ndjson")),
            );
        }
        return follow_logs("host-monitor", &service(), args);
    }
    if args.has("--watch") {
        if args.words != ["status"] || args.format != "ndjson" {
            return emit(
                "host-monitor",
                "status",
                &args.format,
                &Err(fail(2, "watch_requires_status_ndjson")),
            );
        }
        let mut single = args;
        single.options.remove("--watch");
        return watch(&single);
    }
    let command = args.words.join(" ");
    let result = execute(&args);
    // Legacy collector commands own their single result output.
    if result
        .as_ref()
        .is_ok_and(|v| v.get("legacy_output") == Some(&Value::Bool(true)))
    {
        return result
            .as_ref()
            .ok()
            .and_then(|v| v["legacy_exit"].as_u64())
            .unwrap_or(0) as u8;
    }
    let exit = emit("host-monitor", &command, &args.format, &result);
    #[cfg(windows)]
    if args.words == ["setup"]
        && args.has("--installer-session")
        && args.has("--elevated-setup-child")
    {
        pause_installer_setup();
    }
    exit
}
fn execute(args: &Args) -> Result<Value> {
    let words: Vec<_> = args.words.iter().map(String::as_str).collect();
    if args.has("--version") || words == ["version"] {
        return Ok(
            json!({"version":env!("CARGO_PKG_VERSION"),"commit":option_env!("HOST_MONITOR_BUILD_SHA").unwrap_or("unknown"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"protocol":"host-monitoring.client-report.current","cli_schema_version":1,"config_format":host_monitor::config::CONFIG_FORMAT_VERSION,"state_format":pairing::PERSISTED_STATE_FORMAT,"ipc_version":1}),
        );
    }
    if args.has("--state") {
        return Err(fail(2, "state_directory_is_a_config_field"));
    }
    let path = args
        .get("--config")
        .map(PathBuf::from)
        .unwrap_or_else(host_monitor::config::default_config_path);
    if let ["service", action] = words.as_slice() {
        args.validate_options(&["--now"])?;
        if args.has("--now") && !["enable", "disable"].contains(action) {
            return Err(fail(2, "invalid_now_option"));
        }
        return if *action == "status" {
            service().status(args.timeout)
        } else {
            service().change(args, action, &path)
        };
    }
    match words.as_slice() {
        ["setup"] => {
            args.validate_options(&[
                "--interactive",
                "--input-stdin",
                "--server",
                "--installer-session",
                "--elevated-setup-child",
            ])?;
            let c = if config_exists(&path)? || args.has("--config") {
                load(&path).map_err(|error| error.at_step("configuration"))?
            } else {
                new_config(path.clone())
            };
            tokio::runtime::Runtime::new()
                .map_err(|error| storage_error(error).at_step("configuration"))?
                .block_on(setup(args, path, c))
        }
        ["logs"] => {
            args.validate_options(&["--tail", "--since", "--follow"])?;
            if args.has("--follow") {
                return Err(fail(10, "log_follow_not_available"));
            }
            service().logs(args)
        }

        ["config", "init"] => {
            args.validate_options(&["--interactive"])?;
            if config_exists(&path)? {
                return Err(fail(5, "configuration_already_exists"));
            }
            let mut c = new_config(path);
            if args.has("--interactive") {
                let server = host_monitor::pairing_input::validate_server_base(&prompt(
                    "Server origin",
                    false,
                )?)
                .map_err(input_error)?;
                c.endpoint = format!("{server}{}", host_protocol::CLIENT_REPORT_PATH);
            }
            c.validate(ClientCommand::Pair).map_err(input_error)?;
            let _guard = Guard::acquire(&c.state_dir).map_err(runtime_error)?;
            c.persist_durable_config().map_err(storage_error)?;
            Ok(json!({"committed":true,"stored_revision":revision_of(&c)?}))
        }
        ["config", "edit"] => {
            args.validate_options(&[])?;
            let current = load(&path)?;
            let before = revision_of(&current)?;
            let edited = edit_json(&serde_json::to_value(&current).map_err(storage_error)?)?;
            let mut candidate: ClientConfig =
                serde_json::from_value(edited).map_err(input_error)?;
            candidate
                .validate(ClientCommand::Pair)
                .map_err(input_error)?;
            if candidate.state_dir != current.state_dir {
                return Err(fail(2, "state_directory_migration_required"));
            }
            let binding = pairing::local_status(&current).map_err(storage_error)?;
            if (binding.active_report_endpoint.is_some() || binding.progress.is_some())
                && (candidate.endpoint != current.endpoint
                    || candidate.pairing_endpoint != current.pairing_endpoint)
            {
                return Err(fail(5, "server_change_requires_pair_replace"));
            }
            let _guard = Guard::acquire(&current.state_dir).map_err(runtime_error)?;
            if revision_of(&load(&path)?)? != before {
                return Err(fail(5, "revision_conflict"));
            }
            candidate.config_path = Some(path);
            candidate.persist_durable_config().map_err(storage_error)?;
            Ok(
                json!({"committed":true,"previous_revision":before,"stored_revision":revision_of(&candidate)?,"effective_revision":null,"restart_required":true}),
            )
        }
        ["config", action] if ["validate", "diff", "apply"].contains(action) => {
            args.validate_options(&["--file", "--expected-revision"])?;
            let mut candidate = load(Path::new(args.require("--file")?))?;
            candidate
                .validate(ClientCommand::Pair)
                .map_err(input_error)?;
            if *action == "validate" {
                return Ok(json!({"valid":true,"candidate_revision":revision_of(&candidate)?}));
            }
            let current = load(&path)?;
            let _guard = if *action == "apply" {
                Some(Guard::acquire(&current.state_dir).map_err(runtime_error)?)
            } else {
                None
            };
            let current = load(&path)?;
            if candidate.state_dir != current.state_dir {
                return Err(fail(2, "state_directory_migration_required"));
            }
            let binding = pairing::local_status(&current).map_err(storage_error)?;
            if (binding.active_report_endpoint.is_some() || binding.progress.is_some())
                && (candidate.endpoint != current.endpoint
                    || candidate.pairing_endpoint != current.pairing_endpoint)
            {
                return Err(fail(5, "server_change_requires_pair_replace"));
            }
            let before = revision_of(&current)?;
            let after = revision_of(&candidate)?;
            if *action == "apply" {
                if args.require("--expected-revision")? != before {
                    return Err(fail(5, "revision_conflict"));
                }
                candidate.config_path = Some(path);
                candidate.persist_durable_config().map_err(storage_error)?;
                Ok(
                    json!({"committed":true,"stored_revision":after,"effective_revision":null,"restart_required":true}),
                )
            } else {
                let mut old = serde_json::to_value(current).map_err(storage_error)?;
                let mut new = serde_json::to_value(candidate).map_err(storage_error)?;
                redact(&mut old);
                redact(&mut new);
                Ok(
                    json!({"stored_revision":before,"candidate_revision":after,"before":old,"after":new}),
                )
            }
        }
        ["config", "show"] => {
            args.validate_options(&[])?;
            let c = load(&path)?;
            let mut v = serde_json::to_value(&c).map_err(storage_error)?;
            redact(&mut v);
            Ok(json!({"config":v,"stored_revision":revision_of(&c)?}))
        }
        ["pair"] | ["pair", "resume"] | ["pair", "replace"] | ["pair", "recover"] => {
            args.validate_options(&[
                "--interactive",
                "--input-stdin",
                "--server",
                "--expected-binding",
                "--confirm-replace",
            ])?;
            let c = if config_exists(&path)? || args.has("--config") {
                load(&path)?
            } else {
                new_config(path)
            };
            tokio::runtime::Runtime::new()
                .map_err(storage_error)?
                .block_on(pair(args, c))
        }
        ["status"] | ["pair", "status"] | ["queue", "status"] | ["queue", "inspect"] => {
            args.validate_options(&["--watch", "--check"])?;
            let mut selected = vec!["status".into(), "--output".into(), "json".into()];
            if config_exists(&path)? || args.has("--config") {
                selected.extend(["--config".into(), path.to_string_lossy().into_owned()]);
            }
            let (c, _) = ClientConfig::load_from_iter(selected).map_err(input_error)?;
            if args.has("--check") && local_status(&c)?["health"] != "healthy" {
                return Err(fail(12, "business_health_unconfirmed"));
            }
            if words[0] == "queue" {
                queue(&c)
            } else {
                local_status(&c)
            }
        }
        ["queue", "drain"] => {
            args.validate_options(&[])?;
            let mut c = load(&path)?;
            let _guard = Guard::acquire(&c.state_dir).map_err(runtime_error)?;
            let spool = host_monitor::spool::Spool::open(&c.state_dir, c.spool_max_bytes)
                .map_err(|_| fail(5, "delivery_busy"))?;
            let snapshot = pairing::existing_reporter_for_run(&c)
                .map_err(storage_error)?
                .ok_or_else(|| fail(4, "awaiting_pairing"))?;
            let mut host = host_monitor::collectors::load_host_identity(&c.state_dir)
                .map_err(storage_error)?;
            let reporter = snapshot.apply(&mut c, &mut host);
            tokio::runtime::Runtime::new()
                .map_err(storage_error)?
                .block_on(async {
                    tokio::time::timeout(args.timeout, async {
                        use sarmg_client_runtime::DeliveryQueue;
                        while let Some(p) = spool.oldest().map_err(storage_error)? {
                            reporter
                                .send_host_monitoring(&p.report)
                                .await
                                .map_err(|_| fail(6, "delivery_unconfirmed"))?;
                            spool.acknowledge(&p).map_err(storage_error)?;
                        }
                        Ok(json!({"pending_batches":0}))
                    })
                    .await
                    .map_err(|_| fail(9, "queue_drain_timeout"))?
                })
        }
        ["queue", "archive"] => {
            args.validate_options(&["--reason"])?;
            let c = load(&path)?;
            let _guard = Guard::acquire(&c.state_dir).map_err(runtime_error)?;
            archive_queue(&c, args.require("--reason")?)
        }
        [command] if ["run", "once", "probe", "doctor"].contains(command) => {
            args.validate_options(&["--delivery", "--network"])?;
            if args.has("--network") {
                if *command != "doctor" || args.has("--delivery") {
                    return Err(fail(2, "invalid_network_diagnostic"));
                }
                let config = load(&path)?;
                return tokio::runtime::Runtime::new()
                    .map_err(storage_error)?
                    .block_on(host_monitor::transport::network_probe(&config))
                    .map_err(|_| fail(6, "server_unavailable_or_untrusted"));
            }
            let mut normalized = vec![command.to_string()];
            if config_exists(&path)? || args.has("--config") {
                normalized.extend(["--config".into(), path.to_string_lossy().into_owned()]);
            }
            normalized.extend([
                "--output".into(),
                if args.format == "human" {
                    "human"
                } else {
                    "json"
                }
                .into(),
            ]);
            if args.has("--delivery") {
                normalized.push("--delivery".into());
            }
            let (c, cmd) = ClientConfig::load_from_iter(normalized).map_err(input_error)?;
            crate::monitor_app::init_tracing().map_err(storage_error)?;
            let runtime_result = tokio::runtime::Runtime::new()
                .map_err(storage_error)?
                .block_on(crate::monitor_app::execute_config(
                    c,
                    cmd,
                    crate::monitor_app::platform_ready_callback(),
                ));
            if cmd == ClientCommand::Doctor && !args.has("--delivery") {
                return Ok(
                    json!({"legacy_output":true,"legacy_exit":if runtime_result.is_ok(){0}else{12}}),
                );
            }
            runtime_result.map_err(runtime_error)?;
            Ok(json!({"legacy_output":true}))
        }
        _ => Err(fail(2, "unknown_command")),
    }
}

fn new_config(path: PathBuf) -> ClientConfig {
    let mut config = ClientConfig::default();
    config.config_path = Some(path);
    config
}

fn no_args(args: &Args) -> u8 {
    let status_args = Args {
        words: vec!["status".into()],
        options: args.options.clone(),
        format: args.format.clone(),
        timeout: args.timeout,
    };
    let mut result = execute(&status_args);
    if let Ok(value) = &mut result {
        value["next_steps"] = json!([
            "host-monitor setup",
            "host-monitor status",
            "host-monitor service status",
            "host-monitor logs"
        ]);
    }
    emit("host-monitor", "status", &args.format, &result)
}

fn watch(args: &Args) -> u8 {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(_) => return 8,
    };
    rt.block_on(async {let deadline=tokio::time::Instant::now()+args.timeout;loop {
        let result=execute(args);let code=emit("host-monitor","status","ndjson",&result);if code!=0 {return code;}
        tokio::select!{_=tokio::signal::ctrl_c()=>return 130,_=tokio::time::sleep_until(deadline)=>return 0,_=tokio::time::sleep(std::time::Duration::from_secs(1))=>{}}
    }})
}

fn runtime_error(error: anyhow::Error) -> Failure {
    if error.chain().any(|e| {
        e.downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
    }) {
        return fail(3, "permission_denied");
    }
    if cfg!(windows)
        && error.chain().any(|e| {
            e.downcast_ref::<std::io::Error>().is_some_and(|e| {
                e.kind() == std::io::ErrorKind::WouldBlock
                    || matches!(e.raw_os_error(), Some(32 | 33))
            })
        })
    {
        return fail(5, "busy");
    }
    if error.chain().any(|e| {
        matches!(
            e.downcast_ref::<sarmg_client_runtime::Error>(),
            Some(sarmg_client_runtime::Error::AlreadyRunning)
        )
    }) || error.chain().any(|e| {
        matches!(
            e.downcast_ref::<sarmg_client_fs_safety::Error>(),
            Some(sarmg_client_fs_safety::Error::AlreadyLocked(_))
        )
    }) {
        fail(5, "busy")
    } else {
        storage_error(error)
    }
}

#[cfg(test)]
mod setup_tests {
    use super::*;

    #[test]
    fn startup_policy_requires_a_verified_platform_state() {
        assert!(startup_policy_matches(
            &json!({"startup":"automatic"}),
            true
        ));
        assert!(startup_policy_matches(&json!({"startup":"enabled"}), true));
        assert!(startup_policy_matches(&json!({"startup":"manual"}), false));
        assert!(startup_policy_matches(
            &json!({"startup":"disabled"}),
            false
        ));
        assert!(!startup_policy_matches(&json!({"startup":"unknown"}), true));
        assert!(!startup_policy_matches(&json!({"startup":"manual"}), true));
    }

    #[test]
    fn setup_failures_preserve_pairing_and_identify_the_failed_gate() {
        let pairing = json!({"committed":true,"transaction_id":"request-1"});
        let failure = setup_failure(
            fail(11, "service_state_unconfirmed"),
            &pairing,
            "service_runtime",
        );
        assert!(failure.committed);
        assert_eq!(failure.transaction_id.as_deref(), Some("request-1"));
        assert_eq!(failure.step, Some("service_runtime"));
    }

    #[test]
    fn setup_filters_installer_flags_from_nested_commands() {
        let parsed = Args::parse(
            vec![
                "setup".into(),
                "--interactive".into(),
                "--installer-session".into(),
                "--elevated-setup-child".into(),
            ],
            &[
                "--file",
                "--server",
                "--expected-revision",
                "--expected-binding",
            ],
            &["--network", "--delivery", "--confirm-replace"],
        )
        .unwrap();
        let child = setup_args(&parsed, vec!["pair".into()], true);
        assert!(child.has("--interactive"));
        assert!(!child.has("--installer-session"));
        assert!(!child.has("--elevated-setup-child"));
        assert!(
            child
                .validate_options(&["--interactive", "--input-stdin", "--server"])
                .is_ok()
        );
    }

    #[test]
    fn requested_new_server_is_selected_before_any_old_binding_probe() {
        let existing = pairing::LocalPairingStatus {
            progress: None,
            active_report_endpoint: Some(
                "https://offline.example/api/v2/host-monitor/report".into(),
            ),
        };
        let state = requested_server_change(
            &existing,
            Some("https://host.sarmg.org/api/v2/host-monitor/report"),
        );
        assert!(matches!(
            state,
            Some(ExistingBindingState::ServerChanged { configured, requested })
                if configured.contains("offline.example") && requested.contains("host.sarmg.org")
        ));
    }

    #[test]
    fn setup_deadline_never_grants_each_phase_a_fresh_timeout() {
        let deadline = SetupDeadline::new(Duration::from_secs(60));
        assert!(deadline.remaining("pairing").unwrap() <= Duration::from_secs(60));
        let expired = SetupDeadline {
            expires_at: Instant::now(),
        };
        assert_eq!(
            expired.remaining("connection").unwrap_err().code,
            "setup_deadline_exceeded"
        );
    }

    #[cfg(unix)]
    #[test]
    fn queue_archive_preserves_original_report_bytes_and_writes_a_binding_manifest() {
        use std::{fs, os::unix::fs::PermissionsExt};

        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("host-queue-archive-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let host_id = uuid::Uuid::new_v4();
        fs::write(root.join("host-id"), format!("{host_id}\n")).unwrap();
        fs::set_permissions(root.join("host-id"), fs::Permissions::from_mode(0o600)).unwrap();
        let mut config = ClientConfig::default();
        config.state_dir = root.clone();
        {
            let spool = host_monitor::spool::Spool::open(&root, config.spool_max_bytes).unwrap();
            let report = host_monitor::SystemSampler::new().collect(
                host_monitor::collectors::transient_host_identity(host_id),
                config.interval_seconds,
                0,
            );
            spool.enqueue(&report).unwrap();
            assert_eq!(spool.pending_count().unwrap(), 1);
        }
        let archived = archive_queue(&config, "server-state-lost").unwrap();
        let destination = PathBuf::from(archived["archive"].as_str().unwrap());
        assert!(!root.join("spool").exists());
        assert!(destination.join("spool").is_dir());
        let manifest: Value =
            serde_json::from_slice(&fs::read(destination.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["host_id"], host_id.to_string());
        assert_eq!(manifest["reason"], "server-state-lost");
        assert_eq!(manifest["queue"]["pending_batches"], 1);
        assert_eq!(queue(&config).unwrap()["pending_batches"], 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn protected_state_read_errors_keep_not_found_permission_and_io_distinct() {
        let missing =
            state_read_error(std::io::Error::new(std::io::ErrorKind::NotFound, "missing").into());
        assert_eq!(missing.code, "config_missing");
        let denied = state_read_error(
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied").into(),
        );
        assert_eq!(denied.code, "administrator_privileges_required");
        let failed = state_read_error(
            std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short read").into(),
        );
        assert_eq!(failed.code, "state_read_failed");
    }
}
