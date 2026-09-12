use crate::cli_common::*;
use host_monitor::{
    ClientCommand, ClientConfig,
    maintenance::Guard,
    pairing::{self, PairingProgress},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

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
    }
}
fn load(path: &Path) -> Result<ClientConfig> {
    let (mut c, _) = ClientConfig::load_selected_config(Some(path), ClientCommand::Probe)
        .map_err(storage_error)?;
    c.config_path = Some(path.to_owned());
    Ok(c)
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
fn local_status(c: &ClientConfig) -> Result<Value> {
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
    if existing.active_report_endpoint.is_some() && !resume && !replace {
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
    if !resume {
        let pending = queue(&c)?;
        if pending["pending_batches"] != 0 || pending["quarantined"] != 0 {
            return Err(fail(5, "old_binding_queue_not_empty"));
        }
    }
    let operation = async {
        if !resume {
            let host = host_monitor::collectors::transient_host_identity(uuid::Uuid::new_v4());
            let session = pairing::start_or_resume(&c, &host)
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
    let mut options = args.options.clone();
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

fn setup_service_args(args: &Args, now: bool) -> Args {
    let mut options = args.options.clone();
    options.remove("--interactive");
    options.remove("--input-stdin");
    options.remove("--server");
    options.remove("--now");
    if now {
        options.insert("--now".into(), "true".into());
    }
    Args {
        words: vec![],
        options,
        format: args.format.clone(),
        timeout: args.timeout,
    }
}

fn preserve_setup_commit(mut error: Failure, pairing: &Value) -> Failure {
    error.committed = true;
    if error.transaction_id.is_none() {
        error.transaction_id = pairing["transaction_id"].as_str().map(str::to_owned);
    }
    error
}

async fn wait_for_healthy(c: &ClientConfig, timeout: std::time::Duration) -> Result<Value> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let status = local_status(c)?;
        if status["health"] == "healthy" {
            return Ok(status);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(fail(9, "connection_unconfirmed"));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

async fn setup(args: &Args, path: PathBuf, c: ClientConfig) -> Result<Value> {
    if path != service().default_config {
        return Err(fail(2, "service_config_mismatch"));
    }
    c.validate(ClientCommand::Pair)
        .map_err(|_| fail(2, "invalid_configuration"))?;
    let existing = pairing::local_status(&c).map_err(storage_error)?;
    let pairing = if existing.active_report_endpoint.is_some() {
        json!({"committed":true,"already_active":true})
    } else {
        let has_protected_input = args.has("--input-stdin") || args.has("--interactive");
        let resume = existing.progress.is_some()
            && !has_protected_input
            && args.get("--server").is_none()
            && !args.has("--non-interactive");
        let pair_words = if resume {
            vec!["pair".into(), "resume".into()]
        } else {
            vec!["pair".into()]
        };
        let interactive = !resume && !args.has("--input-stdin") && !args.has("--non-interactive");
        let pair_args = setup_args(args, pair_words, interactive);
        pair(&pair_args, c).await?
    };
    let config = match load(&path) {
        Ok(config) => config,
        Err(error) => return Err(preserve_setup_commit(error, &pairing)),
    };
    let interactive = !args.has("--non-interactive") && !args.has("--input-stdin");
    let (enable, start, verify) = if interactive {
        (
            ask_yes_no("Enable service at system startup?", true)
                .map_err(|error| preserve_setup_commit(error, &pairing))?,
            ask_yes_no("Start the service now?", true)
                .map_err(|error| preserve_setup_commit(error, &pairing))?,
            ask_yes_no("Verify the connection now?", true)
                .map_err(|error| preserve_setup_commit(error, &pairing))?,
        )
    } else {
        // A protected stdin document is already an explicit deployment action.
        // Complete the same safe install contract without an interactive prompt.
        (true, true, true)
    };
    let service_result = if enable {
        service()
            .change(&setup_service_args(args, start), "enable", &path)
            .map_err(|error| preserve_setup_commit(error, &pairing))?
    } else if start {
        service()
            .change(&setup_service_args(args, false), "start", &path)
            .map_err(|error| preserve_setup_commit(error, &pairing))?
    } else {
        service()
            .status(args.timeout)
            .map_err(|error| preserve_setup_commit(error, &pairing))?
    };
    let verification = if verify && service_result["state"] == "running" {
        wait_for_healthy(&config, args.timeout)
            .await
            .map_err(|error| preserve_setup_commit(error, &pairing))?
    } else {
        json!({"state":"skipped","reason":if verify {"service_not_running"} else {"not_requested"}})
    };
    Ok(json!({
        "setup":"completed",
        "pairing":pairing,
        "service":service_result,
        "verification":verification
    }))
}

pub fn entry(raw: Vec<String>) -> u8 {
    let args = match Args::parse(raw) {
        Ok(a) => a,
        Err(e) => return emit("host-monitor", "parse", "json", &Err(e)),
    };
    if args.has("--help") {
        println!(
            "host-monitor: setup; config init|show|validate|diff|apply; pair [status|resume|replace]; queue status|inspect|drain; status; doctor; service status|start|stop|restart|enable|disable; run; once; probe; version\nGlobal: --config ABSOLUTE_PATH --format human|json|ndjson --non-interactive --timeout 60s --no-color\nsetup/pair uses --interactive or --input-stdin JSON containing server and authorization_code. setup completes pairing, service startup policy and connection verification. Never pass secrets as arguments.\nconfig apply requires --file and --expected-revision. Stop the service before writes."
        );
        return 0;
    }
    if args.words.is_empty() && !args.has("--version") {
        return no_args(&args);
    }
    if (args.has("--version") || args.words == ["version"]) && args.format == "human" {
        println!("host-monitor {}", env!("CARGO_PKG_VERSION"));
        return 0;
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
    emit("host-monitor", &command, &args.format, &result)
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
            args.validate_options(&["--interactive", "--input-stdin", "--server"])?;
            let c = if path.exists() || args.has("--config") {
                load(&path)?
            } else {
                new_config(path.clone())
            };
            tokio::runtime::Runtime::new()
                .map_err(storage_error)?
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
            if path.exists() {
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
        ["pair"] | ["pair", "resume"] | ["pair", "replace"] => {
            args.validate_options(&[
                "--interactive",
                "--input-stdin",
                "--server",
                "--expected-binding",
                "--confirm-replace",
            ])?;
            let c = if path.exists() || args.has("--config") {
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
            if path.exists() || args.has("--config") {
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
        [command] if ["run", "once", "probe", "doctor"].contains(command) => {
            args.validate_options(&["--delivery", "--network"])?;
            if args.has("--network") {
                if *command != "doctor" || args.has("--delivery") {
                    return Err(fail(2, "invalid_network_diagnostic"));
                }
                let config = load(&path)?;
                return host_monitor::transport::network_probe(&config)
                    .map_err(|_| fail(6, "server_unavailable_or_untrusted"));
            }
            let mut normalized = vec![command.to_string()];
            if path.exists() || args.has("--config") {
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
