#![cfg(all(feature = "desktop", unix))]

// Compile the production application helper in a separate test executable:
// xcsc's runtime-status snapshot is process-global, so publishing it in
// the binary's parallel unit tests would mix observations from other reporters.
mod delivery {
    use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
    use tracing::info;
    use uuid::Uuid;
    use xsoc::{
        ClientCommand, ClientConfig, pairing,
        service::{ShutdownSignal, shutdown_channel},
        transport::Reporter,
    };

    include!("../src/monitor_app/delivery/reporter.rs");

    fn stage_activation(config: &ClientConfig, instance_id: Uuid, token: &str) {
        let path = config.state_dir.join("pairing-state.json");
        let request_id = Uuid::new_v4();
        let journal = serde_json::json!({
            "phase": "activating", "version": "1.0.0",
            "generation": Uuid::new_v4(), "request_id": request_id,
            "activation_url": format!("https://127.0.0.1:9/activate/{request_id}"),
            "expires_at": chrono::Utc::now() + chrono::TimeDelta::minutes(10),
            "poll_interval": 2, "instance_id": instance_id,
            "pairing_endpoint": config.pairing_endpoint(),
            "report_endpoint": config.endpoint, "bearer_secret": token,
        });
        fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[tokio::test]
    async fn credential_rotation_requires_a_new_runtime_acknowledgement() {
        // Use a short physical path, including on macOS, for the Unix socket.
        let directory = tempfile::Builder::new()
            .prefix("host-status-")
            .tempdir_in(std::path::Path::new("/tmp").canonicalize().unwrap())
            .unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = ClientConfig::default();
        config.state_dir = directory.path().to_path_buf();
        config.endpoint = "https://127.0.0.1:9/api/v1/xsoc/report".into();
        stage_activation(&config, Uuid::new_v4(), &"a".repeat(64));
        pairing::poll_existing(&config).await.unwrap();
        let mut host = xsoc::collectors::load_host_identity(&config.state_dir).unwrap();
        let _publisher = xsoc::runtime_status::publish(
            &config.state_dir,
            host.id.clone(),
            xcsc::cli::revision(&serde_json::to_vec(&config).unwrap()),
        )
        .unwrap();
        let (_sender, shutdown) = shutdown_channel();
        let mut reporter = prepare_reporter(&mut config, &mut host, ClientCommand::Run, &shutdown)
            .await
            .unwrap()
            .unwrap();
        let collection_at = chrono::Utc::now().timestamp();
        xsoc::runtime_status::observe("last_collection_at", serde_json::json!(collection_at));

        for (index, change) in ["identity", "endpoint", "credential", "configuration"]
            .into_iter()
            .enumerate()
        {
            // Publish the same observations as a successful delivery, then
            // read them through the real socket rather than a synthetic Value.
            for (field, value) in [
                (
                    "last_ack_at",
                    serde_json::json!(chrono::Utc::now().timestamp()),
                ),
                ("last_delivery_attempt_at", serde_json::json!(collection_at)),
                ("last_delivery_result", serde_json::json!("accepted")),
                ("last_http_status", serde_json::json!(202)),
                ("last_error_code", serde_json::Value::Null),
            ] {
                xsoc::runtime_status::observe(field, value);
            }
            let before = xsoc::runtime_status::read(&config.state_dir, Some(&host.id))
                .expect("the status endpoint exposes the prior acknowledgement");
            assert!(before["last_ack_at"].as_i64().is_some());
            assert_eq!(before["last_delivery_result"], "accepted");
            if change == "credential" {
                // A later rejection must not carry over either. The earlier
                // ACK remains in the old snapshot until this rotation.
                xsoc::runtime_status::observe("last_error_code", serde_json::json!("unauthorized"));
                xsoc::runtime_status::observe("last_http_status", serde_json::json!(401));
                xsoc::runtime_status::observe(
                    "last_delivery_result",
                    serde_json::json!("rejected"),
                );
            }

            let instance_id = if change == "identity" {
                Uuid::new_v4()
            } else {
                Uuid::parse_str(&host.id).unwrap()
            };
            if change == "endpoint" {
                config.endpoint = "https://127.0.0.1:10/api/v1/xsoc/report".into();
            }
            if change == "configuration" {
                config.interval_seconds += 1;
            }
            stage_activation(&config, instance_id, &format!("{:064x}", index + 2));
            pairing::poll_existing(&config).await.unwrap();
            let snapshot =
                pairing::refresh_reporter_snapshot(&config, reporter.credential_revision())
                    .unwrap()
                    .unwrap();
            reporter = apply_reporter_snapshot(snapshot, &mut config, &mut host).unwrap();

            let current = xsoc::runtime_status::read(&config.state_dir, Some(&host.id))
                .expect("the status endpoint is bound to the replacement reporter");
            assert_eq!(current["binding_generation"], instance_id.to_string());
            assert_eq!(current["service_epoch"], before["service_epoch"]);
            assert_eq!(
                current["effective_revision"],
                xcsc::cli::revision(&serde_json::to_vec(&config).unwrap())
            );
            assert_eq!(current["last_collection_at"], collection_at);
            for field in [
                "last_ack_at",
                "last_delivery_attempt_at",
                "last_delivery_result",
                "last_http_status",
                "last_error_code",
            ] {
                assert!(
                    current[field].is_null(),
                    "{change} retained {field}: {current}"
                );
            }
        }
    }
}
