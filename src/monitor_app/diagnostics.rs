use super::*;

#[derive(Debug, Serialize)]
struct DiagnosticCheck {
    id: &'static str,
    status: &'static str,
    code: Option<&'static str>,
    message: String,
    remediation: Option<String>,
    duration_ms: u64,
}

impl DiagnosticCheck {
    fn new(
        id: &'static str,
        status: &'static str,
        code: Option<&'static str>,
        message: impl Into<String>,
        remediation: Option<impl Into<String>>,
        started: Instant,
    ) -> Self {
        Self {
            id,
            status,
            code,
            message: message.into(),
            remediation: remediation.map(Into::into),
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

struct HostInspection {
    id: Option<String>,
    check: DiagnosticCheck,
}

fn inspect_host_identity(config: &ClientConfig) -> HostInspection {
    let started = Instant::now();
    let path = config.state_dir.join("host-id");
    match host_monitor::client_identity::load(&config.state_dir) {
        Ok(identity) => HostInspection {
            id: Some(identity.instance_id().to_owned()),
            check: DiagnosticCheck::new(
                "identity",
                "ok",
                None,
                "host identity is readable and valid",
                None::<String>,
                started,
            ),
        },
        Err(host_monitor::client_identity::IdentityLoadError::Invalid) => HostInspection {
            id: None,
            check: DiagnosticCheck::new(
                "identity",
                "error",
                Some("identity_invalid"),
                "host identity is not valid current identity data",
                Some("repair the state directory or pair this host again"),
                started,
            ),
        },
        Err(host_monitor::client_identity::IdentityLoadError::State(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            HostInspection {
                id: None,
                check: DiagnosticCheck::new(
                    "identity",
                    "missing",
                    Some("identity_missing"),
                    "host identity has not been created yet",
                    Some("pair this host before expecting authenticated reports"),
                    started,
                ),
            }
        }
        Err(error) => HostInspection {
            id: None,
            check: DiagnosticCheck::new(
                "identity",
                "error",
                Some("identity_unreadable"),
                format!("failed to read {}: {error}", path.display()),
                Some("check the state-directory owner and permissions"),
                started,
            ),
        },
    }
}

fn inspect_configuration(config: &ClientConfig, configured: bool) -> DiagnosticCheck {
    let started = Instant::now();
    match config.validate_for_diagnostics() {
        Err(error) => DiagnosticCheck::new(
            "configuration",
            "error",
            Some("config_invalid"),
            error.to_string(),
            Some("repair the configuration file, then run status again"),
            started,
        ),
        Ok(()) if configured => DiagnosticCheck::new(
            "configuration",
            "ok",
            None,
            "configuration file is present and its effective settings are valid",
            None::<String>,
            started,
        ),
        Ok(()) => DiagnosticCheck::new(
            "configuration",
            "missing",
            Some("config_missing"),
            "configuration file is not present",
            Some("pair this host to create its private configuration"),
            started,
        ),
    }
}

fn inspect_tls(config: &ClientConfig) -> DiagnosticCheck {
    let started = Instant::now();
    match host_monitor::transport::validate_local_tls(config) {
        Ok(()) => DiagnosticCheck::new(
            "tls",
            "ok",
            None,
            "local TLS inputs and client construction passed; no connection or handshake was attempted",
            None::<String>,
            started,
        ),
        Err(error) => DiagnosticCheck::new(
            "tls",
            "error",
            Some("tls_configuration_invalid"),
            error.to_string(),
            Some(
                "check platform identity support, password, certificate contents, owner, permissions and file sizes; use doctor --delivery for an explicit delivery test",
            ),
            started,
        ),
    }
}

struct CredentialInspection {
    present: bool,
    check: DiagnosticCheck,
}

fn inspect_credential(config: &ClientConfig) -> CredentialInspection {
    let started = Instant::now();
    let path = config.state_dir.join("client-token");
    match host_monitor::transport::stored_credential_is_nonempty(config) {
        Ok(true) => CredentialInspection {
            present: true,
            check: DiagnosticCheck::new(
                "credential",
                "ok",
                None,
                "the private host credential is readable",
                None::<String>,
                started,
            ),
        },
        Ok(_) => CredentialInspection {
            present: false,
            check: DiagnosticCheck::new(
                "credential",
                "error",
                Some("credential_empty"),
                format!("{} is empty", path.display()),
                Some("pair this host again"),
                started,
            ),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => CredentialInspection {
            present: false,
            check: DiagnosticCheck::new(
                "credential",
                "missing",
                Some("credential_missing"),
                "no host credential is stored yet",
                Some("pair this host before expecting authenticated reports"),
                started,
            ),
        },
        Err(error) => CredentialInspection {
            present: false,
            check: DiagnosticCheck::new(
                "credential",
                "error",
                Some("credential_unreadable"),
                format!("failed to read {}: {error}", path.display()),
                Some("check the state-directory owner and permissions"),
                started,
            ),
        },
    }
}

#[derive(Default, Serialize)]
struct SpoolInspection {
    pending_batches: usize,
    invalid_batches: usize,
    identity_mismatch_batches: usize,
    total_bytes: u64,
    #[serde(skip)]
    check: Option<DiagnosticCheck>,
}

fn inspect_spool(config: &ClientConfig) -> SpoolInspection {
    let started = Instant::now();
    let path = config.state_dir.join("spool");
    let limits = sarmg_client_runtime::SpoolLimits {
        max_record_bytes: host_monitor::model::CLIENT_REPORT_MAX_BODY_BYTES,
        max_entries: sarmg_client_runtime::MAX_SPOOL_ENTRIES,
        max_bytes: config.spool_max_bytes,
    };
    match sarmg_client_runtime::Spool::inspect_existing(&path, limits) {
        Ok(health) => SpoolInspection {
            pending_batches: health.spool_entries,
            invalid_batches: health.quarantined_entries,
            identity_mismatch_batches: health.identity_mismatch_entries,
            total_bytes: health.spool_bytes,
            check: Some(DiagnosticCheck::new(
                "spool",
                if health.healthy { "ok" } else { "error" },
                if health.identity_mismatch_entries > 0 {
                    Some("spool_identity_mismatch")
                } else {
                    (!health.healthy).then_some("spool_quarantined")
                },
                format!(
                    "{} pending, {} quarantined ({} identity mismatches), {} bytes; inventory only, payloads not verified",
                    health.spool_entries,
                    health.quarantined_entries,
                    health.identity_mismatch_entries,
                    health.spool_bytes
                ),
                (!health.healthy)
                    .then_some("inspect quarantined records; diagnostics do not delete evidence"),
                started,
            )),
        },
        Err(sarmg_client_runtime::Error::Filesystem(sarmg_client_fs_safety::Error::Io(error)))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            SpoolInspection {
                check: Some(DiagnosticCheck::new(
                    "spool",
                    "missing",
                    None,
                    "the spool is absent or changed during inspection",
                    None::<String>,
                    started,
                )),
                ..Default::default()
            }
        }
        Err(error) => SpoolInspection {
            check: Some(DiagnosticCheck::new(
                "spool",
                "error",
                Some("spool_unreadable"),
                format!("failed to inspect {}: {error}", path.display()),
                Some(
                    "retry if the client is writing; otherwise check private directory safety, capacity, and disk health",
                ),
                started,
            )),
            ..Default::default()
        },
    }
}

pub(super) fn print_local_status(config: &ClientConfig) -> anyhow::Result<()> {
    let configured = config
        .config_path
        .as_ref()
        .is_some_and(|path| path.is_file());
    let config_check = inspect_configuration(config, configured);
    let tls_check = inspect_tls(config);
    let host = inspect_host_identity(config);
    let credential = inspect_credential(config);
    let mut spool = inspect_spool(config);
    let spool_check = spool
        .check
        .take()
        .expect("spool inspection always produces a check");

    let pairing_result = pairing::local_status(config);
    let authorization_result = pairing::local_auth_state(config);
    let pairing_error = pairing_result.as_ref().err().map(ToString::to_string);
    let authorization_error = authorization_result.as_ref().err().map(ToString::to_string);
    let pairing_status = pairing_result.ok();
    let pairing = pairing_status
        .as_ref()
        .and_then(|status| status.progress.as_ref());
    let active_endpoint = pairing_status
        .as_ref()
        .and_then(|status| status.active_report_endpoint.as_deref());
    let status_endpoint = pairing_error
        .is_none()
        .then(|| active_endpoint.unwrap_or(config.endpoint.as_str()));
    let authorization = authorization_result.ok().flatten();
    let reauth_required = authorization.as_ref().is_some_and(|state| {
        state.status == sarmg_client_runtime::CredentialAuthorization::ReauthorizationRequired
    });
    let pairing_pending = pairing.as_ref().is_some_and(|progress| {
        matches!(
            progress,
            PairingProgress::Creating { .. } | PairingProgress::Waiting(_)
        )
    });
    let has_error = [
        config_check.status,
        tls_check.status,
        host.check.status,
        credential.check.status,
        spool_check.status,
    ]
    .contains(&"error")
        || pairing_error.is_some()
        || authorization_error.is_some();
    let overall_state = if has_error {
        "degraded"
    } else if reauth_required {
        "reauth_required"
    } else if pairing_pending {
        "pairing"
    } else if configured && host.id.is_some() && credential.present {
        "configured"
    } else {
        "unconfigured"
    };
    let config_status = config_check.status;
    let tls_status = tls_check.status;
    let (binding_status, binding_code, binding_message) = if let Some(error) = &pairing_error {
        (
            "error",
            Some("active_pairing_snapshot_invalid"),
            error.clone(),
        )
    } else if active_endpoint.is_some() {
        (
            "ok",
            None,
            "active credential endpoint binding is readable and current".into(),
        )
    } else {
        (
            "skipped",
            None,
            "there is no Active pairing endpoint to inspect".into(),
        )
    };
    let next_action = match overall_state {
        "degraded" => "repair the failed local check, then run `host-monitor doctor`",
        "reauth_required" => {
            "create a new pairing invitation in Host Monitoring and pair this host again"
        }
        "pairing" => "complete or resume the saved browser pairing request",
        "unconfigured" => "run `host-monitor pair --server https://your-console`",
        _ => "run `host-monitor doctor --delivery` for an explicit end-to-end delivery test",
    };
    let checks = serde_json::json!({
        "configuration": config_check,
        "tls": tls_check,
        "identity": host.check,
        "credential": credential.check,
        "spool": spool_check,
        "pairing": {
            "status": if pairing_error.is_some() { "error" } else { "ok" },
            "code": pairing_error.as_ref().map(|_| "pairing_state_invalid"),
            "message": pairing_error
        },
        "active_binding": {
            "status": binding_status,
            "code": binding_code,
            "message": binding_message
        },
        "authorization": {
            "status": if authorization_error.is_some() { "error" } else { "ok" },
            "code": authorization_error.as_ref().map(|_| "authorization_state_invalid"),
            "message": authorization_error
        }
    });
    let snapshot = serde_json::json!({
        "schema_version": 1,
        "command": "status",
        "status": overall_state,
        "configured": configured,
        "config": config.config_path,
        "endpoint": status_endpoint,
        "state_dir": config.state_dir,
        "host_id": host.id,
        "credential_present": credential.present,
        "spool_pending_batches": spool.pending_batches,
        "spool_invalid_batches": spool.invalid_batches,
        "spool_identity_mismatch_batches": spool.identity_mismatch_batches,
        "spool_bytes": spool.total_bytes,
        "pairing": pairing,
        "authorization": authorization,
        "checks": &checks,
        "next_action": next_action
    });
    match config.output_mode {
        OutputMode::Json => println!("{}", serde_json::to_string_pretty(&snapshot)?),
        OutputMode::Human => {
            println!("host-monitor: {overall_state}");
            println!("  Configuration: {config_status}");
            println!("  TLS (local inputs only): {tls_status}");
            println!(
                "  Identity: {}",
                snapshot["host_id"].as_str().unwrap_or("not available")
            );
            println!(
                "  Credential: {}",
                if credential.present {
                    "present"
                } else {
                    "missing"
                }
            );
            println!("  Endpoint: {}", status_endpoint.unwrap_or("not available"));
            println!(
                "  Spool: {} pending, {} quarantined ({} identity mismatches), {} bytes",
                spool.pending_batches,
                spool.invalid_batches,
                spool.identity_mismatch_batches,
                spool.total_bytes
            );
            println!("  Next: {next_action}");
        }
    }
    Ok(())
}

pub(super) async fn run_read_only_doctor(config: &ClientConfig) -> anyhow::Result<()> {
    let mut checks = Vec::new();
    checks.push(inspect_tls(config));

    let started = Instant::now();
    checks.push(match config.validate_for_diagnostics() {
        Ok(()) => DiagnosticCheck::new(
            "configuration",
            "ok",
            None,
            "effective configuration is valid",
            None::<String>,
            started,
        ),
        Err(error) => DiagnosticCheck::new(
            "configuration",
            "error",
            Some("config_invalid"),
            error.to_string(),
            Some("repair the reported setting before starting the service"),
            started,
        ),
    });

    let started = Instant::now();
    checks.push(match fs::metadata(&config.state_dir) {
        Ok(metadata) if metadata.is_dir() => DiagnosticCheck::new(
            "state_directory",
            "ok",
            None,
            format!("{} is accessible", config.state_dir.display()),
            None::<String>,
            started,
        ),
        Ok(_) => DiagnosticCheck::new(
            "state_directory",
            "error",
            Some("state_directory_invalid"),
            format!("{} is not a directory", config.state_dir.display()),
            Some("restore the package-managed private state directory"),
            started,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DiagnosticCheck::new(
            "state_directory",
            "missing",
            None,
            "state directory has not been created yet",
            Some("pair this host or start the packaged service"),
            started,
        ),
        Err(error) => DiagnosticCheck::new(
            "state_directory",
            "error",
            Some("state_directory_unreadable"),
            format!("failed to inspect {}: {error}", config.state_dir.display()),
            Some("check the service account, owner, permissions, and disk health"),
            started,
        ),
    });

    let host = inspect_host_identity(config);
    let diagnostic_id = host
        .id
        .as_deref()
        .and_then(|value| Uuid::parse_str(value).ok())
        .unwrap_or_else(Uuid::new_v4);
    checks.push(host.check);
    checks.push(inspect_credential(config).check);
    let mut spool = inspect_spool(config);
    checks.push(
        spool
            .check
            .take()
            .expect("spool inspection always produces a check"),
    );

    let started = Instant::now();
    let collection_host = transient_host_identity(diagnostic_id);
    let mut sampler = SystemSampler::new();
    let report = sampler.collect(
        collection_host,
        config.slow_interval_seconds,
        spool.pending_batches as u64,
    );
    let capabilities = report.capabilities.len();
    let collector_errors = report.client.collector_errors;
    checks.push(DiagnosticCheck::new(
        "local_collection",
        if collector_errors == 0 {
            "ok"
        } else {
            "warning"
        },
        (collector_errors != 0).then_some("collector_degraded"),
        format!(
            "local snapshot completed with {capabilities} capabilities and {collector_errors} collector errors"
        ),
        (collector_errors != 0)
            .then_some("inspect capability details with `host-monitor probe --output json`"),
        started,
    ));
    checks.push(DiagnosticCheck::new(
        "server_delivery",
        "skipped",
        None,
        "no report was sent; read-only doctor never drains the spool or changes credentials",
        Some("use `host-monitor doctor --delivery` for an explicit end-to-end test"),
        Instant::now(),
    ));

    let has_errors = checks.iter().any(|check| check.status == "error");
    let has_warnings = checks
        .iter()
        .any(|check| matches!(check.status, "warning" | "missing"));
    let status = if has_errors {
        "unhealthy"
    } else if has_warnings {
        "attention"
    } else {
        "healthy"
    };
    let result = serde_json::json!({
        "schema_version": 1,
        "command": "doctor",
        "status": status,
        "mode": "read_only",
        "checks": &checks,
        "next_action": if has_errors {
            "repair the failed checks and run doctor again"
        } else {
            "use --delivery only when a real server write is intended"
        }
    });
    match config.output_mode {
        OutputMode::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        OutputMode::Human => {
            println!("host-monitor doctor: {status} (read-only)");
            for check in &checks {
                println!("  {:<18} {:<9} {}", check.id, check.status, check.message);
                if let Some(remediation) = &check.remediation {
                    println!("    Next: {remediation}");
                }
            }
        }
    }
    if has_errors {
        anyhow::bail!("one or more read-only diagnostic checks failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spool_inspection_does_not_create_state_or_remove_quarantines() {
        let directory = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-spool-diagnostics-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let mut config = ClientConfig::default();
        config.state_dir = directory.join("state");
        let absent = inspect_spool(&config);
        assert_eq!(absent.check.unwrap().status, "missing");
        assert!(!config.state_dir.exists());
        sarmg_client_fs_safety::PrivateDirectory::create(&config.state_dir).unwrap();
        let path = config.state_dir.join("spool");
        let spool = sarmg_client_runtime::Spool::open(
            &path,
            sarmg_client_runtime::SpoolLimits {
                max_record_bytes: host_monitor::model::CLIENT_REPORT_MAX_BODY_BYTES,
                max_entries: sarmg_client_runtime::MAX_SPOOL_ENTRIES,
                max_bytes: config.spool_max_bytes,
            },
        )
        .unwrap();
        let id = spool
            .enqueue(
                sarmg_client_runtime::ContractId::new("example.current").unwrap(),
                1,
                sarmg_client_runtime::BoundedBytes::new(vec![1], 1).unwrap(),
            )
            .unwrap();
        spool
            .quarantine(&id, sarmg_client_runtime::QuarantineReason::Corrupt)
            .unwrap();
        let inspection = inspect_spool(&config);
        assert_eq!(inspection.pending_batches, 0);
        assert_eq!(inspection.invalid_batches, 1);
        assert!(inspection.total_bytes > 1);
        assert_eq!(inspection.check.unwrap().code, Some("spool_quarantined"));
        assert_eq!(spool.doctor().unwrap().quarantined_entries, 1);
        let id = spool
            .enqueue(
                sarmg_client_runtime::ContractId::new("example.current").unwrap(),
                2,
                sarmg_client_runtime::BoundedBytes::new(vec![2], 1).unwrap(),
            )
            .unwrap();
        spool
            .quarantine(
                &id,
                sarmg_client_runtime::QuarantineReason::IdentityMismatch,
            )
            .unwrap();
        let snapshot = || {
            let mut entries = fs::read_dir(&path)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    let metadata = entry.metadata().unwrap();
                    // Windows byte-range locks exclude reads of empty coordination
                    // files. Empty content is fully described by its zero length;
                    // compare every nonempty byte without naming SDK-private files.
                    let bytes = if metadata.len() == 0 {
                        None
                    } else {
                        Some(fs::read(entry.path()).unwrap())
                    };
                    (
                        entry.file_name(),
                        metadata.len(),
                        metadata.modified().unwrap(),
                        bytes,
                    )
                })
                .collect::<Vec<_>>();
            entries.sort();
            entries
        };
        let before = snapshot();
        let inspection = inspect_spool(&config);
        assert_eq!(inspection.pending_batches, 0);
        assert_eq!(inspection.invalid_batches, 2);
        assert_eq!(inspection.identity_mismatch_batches, 1);
        assert_eq!(
            inspection.check.unwrap().code,
            Some("spool_identity_mismatch")
        );
        assert_eq!(snapshot(), before);
        drop(spool);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn host_inspection_rejects_noncanonical_uuid_text() {
        let directory = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!(
                "host-monitoring-diagnostic-host-{}",
                Uuid::new_v4()
            ));
        fs::create_dir_all(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(
            directory.join("host-id"),
            "BBBBBBBB-BBBB-4BBB-8BBB-BBBBBBBBBBBB",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.join("host-id"), fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let mut config = ClientConfig::default();
        config.state_dir = directory.clone();

        let inspection = inspect_host_identity(&config);
        fs::remove_dir_all(directory).unwrap();

        assert!(inspection.id.is_none());
        assert_eq!(inspection.check.status, "error");
        assert_eq!(inspection.check.code, Some("identity_invalid"));
    }

    #[cfg(unix)]
    #[test]
    fn identity_diagnostics_reject_unsafe_state_without_creating_or_repairing_it() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-identity-safety-{}", Uuid::new_v4()));
        let mut config = ClientConfig::default();
        config.state_dir = directory.clone();
        assert_eq!(
            inspect_host_identity(&config).check.code,
            Some("identity_missing")
        );
        assert!(!directory.exists());
        let root = sarmg_client_fs_safety::PrivateDirectory::create(&directory).unwrap();
        let identity = Uuid::new_v4().to_string();
        let name = sarmg_client_fs_safety::EntryName::new("host-id").unwrap();
        sarmg_client_fs_safety::AtomicFile::replace(
            &root,
            &name.as_relative(),
            identity.as_bytes(),
        )
        .unwrap();
        assert_eq!(
            inspect_host_identity(&config).id.as_deref(),
            Some(identity.as_str())
        );
        let path = directory.join("host-id");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            inspect_host_identity(&config).check.code,
            Some("identity_unreadable")
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o644
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let oversized = format!("{identity}{}", " ".repeat(129));
        fs::write(&path, &oversized).unwrap();
        assert_eq!(
            inspect_host_identity(&config).check.code,
            Some("identity_unreadable")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), oversized);
        fs::rename(&path, directory.join("victim")).unwrap();
        symlink(directory.join("victim"), &path).unwrap();
        assert_eq!(
            inspect_host_identity(&config).check.code,
            Some("identity_unreadable")
        );
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        drop(root);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn status_configuration_check_validates_effective_settings() {
        let mut config = ClientConfig::default();
        config.interval_seconds = 0;

        let check = inspect_configuration(&config, true);

        assert_eq!(check.status, "error");
        assert_eq!(check.code, Some("config_invalid"));
        assert!(check.message.contains("interval_seconds"));
    }
}
