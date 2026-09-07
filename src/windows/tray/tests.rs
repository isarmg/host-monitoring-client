#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_dtos_accept_only_the_current_wire_shape() {
        let current_preferences = format!(
            r#"{{"application_version":"{}","server":""}}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert!(serde_json::from_str::<TrayPreferences>(&current_preferences).is_ok());
        assert!(serde_json::from_str::<TrayPreferences>(r#"{"server":""}"#).is_err());
        assert!(
            serde_json::from_str::<TrayPreferences>(
                r#"{"application_version":"0.0.0","server":""}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<TrayPreferences>(
                r#"{"application_version":"0.0.0","server":"","unknown_extension":true}"#
            )
            .is_err()
        );

        assert!(
            serde_json::from_str::<PairRequest>(
                r#"{"server":"https://server.example","activation_code":"secret"}"#
            )
            .is_ok()
        );
        assert!(
            serde_json::from_str::<PairRequest>(
                r#"{"server":"https://server.example","name":"host","activation_code":"secret"}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<ConnectionRequest>(r#"{"server":""}"#).is_ok());
        assert!(serde_json::from_str::<ConnectionRequest>(r#"{}"#).is_err());
        assert!(serde_json::from_str::<StateRequest>(r#"{}"#).is_ok());
        assert!(serde_json::from_str::<StateRequest>(r#"{"unknown_extension":true}"#).is_err());
        assert!(serde_json::from_str::<ServiceRequest>(r#"{"action":"start","extra":1}"#).is_err());
        assert!(serde_json::from_str::<OperationRequest>(r#"{"id":"id","extra":1}"#).is_err());

        assert!(
            serde_json::from_str::<PairIpcMessage>(
                r#"{"generation":"generation","request_id":"request","activation_url":"https://server.example/activate/request","pairing_endpoint":"https://server.example/api/v2/host-monitor/pairing-requests"}"#
            )
            .is_ok()
        );
        assert!(
            serde_json::from_str::<PairIpcMessage>(
                r#"{"generation":"generation","request_id":"request","activation_url":"https://server.example/activate/request","pairing_endpoint":"https://server.example/api/v2/host-monitor/pairing-requests","unknown_extension":true}"#
            )
            .is_err()
        );

        let waiting_event = format!(
            r#"{{"event":"pairing_waiting","version":"{}","request_id":"request","generation":"generation","activation_url":"https://server.example/activate/request","pairing_endpoint":"https://server.example/api/v2/host-monitor/pairing-requests","expires_at":"2026-08-20T00:00:00Z","poll_interval":2}}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert!(serde_json::from_str::<PairEvent>(&waiting_event).is_ok());
        let waiting_event_without_expiry = format!(
            r#"{{"event":"pairing_waiting","version":"{}","request_id":"request","generation":"generation","activation_url":"https://server.example/activate/request","pairing_endpoint":"https://server.example/api/v2/host-monitor/pairing-requests","poll_interval":2}}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert!(serde_json::from_str::<PairEvent>(&waiting_event_without_expiry).is_err());
        let paired_event_with_unknown_field = format!(
            r#"{{"event":"paired","version":"{}","request_id":"request","instance_id":"instance","endpoint":"https://server.example/api/v2/host-monitor/report","unknown_extension":true}}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert!(serde_json::from_str::<PairEvent>(&paired_event_with_unknown_field).is_err());
        assert!(
            serde_json::from_str::<PairEvent>(r#"{"event":"pairing_cancelled","version":1}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<PairEvent>(r#"{"event":"pairing_cancelled","version":"0.0.0"}"#)
                .is_err()
        );
    }

    #[test]
    fn existing_preferences_can_be_atomically_replaced() {
        let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
            "host-monitoring-tray-preferences-{}",
            random_secret()
        ));
        let path = directory.join("tray.json");
        let first = TrayPreferences {
            application_version: CurrentPackageVersion,
            server: "https://first.example".into(),
        };
        let second = TrayPreferences {
            application_version: CurrentPackageVersion,
            server: "https://second.example".into(),
        };
        save_preferences(&path, &first).unwrap();
        save_preferences(&path, &second).unwrap();
        let loaded = load_preferences(&path).unwrap();
        assert_eq!(loaded.server, second.server);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pairing_ipc_requires_canonical_uuid_text() {
        let generation = uuid::Uuid::new_v4().to_string();
        let request_id = uuid::Uuid::new_v4().to_string();
        let server = "https://server.example";
        let message = PairIpcMessage {
            generation: generation.clone(),
            request_id: request_id.clone(),
            activation_url: format!("{server}/activate/{request_id}"),
            pairing_endpoint: format!("{server}/api/v2/host-monitor/pairing-requests"),
        };
        validate_pair_ipc_message(&message, server).unwrap();

        let uppercase = PairIpcMessage {
            generation: generation.to_uppercase(),
            ..message
        };
        assert!(validate_pair_ipc_message(&uppercase, server).is_err());
    }

    #[test]
    fn bounded_line_reader_rejects_before_growing_past_the_limit() {
        let input = vec![b'x'; MAX_NDJSON_LINE_BYTES + 1];
        let mut reader = BufReader::with_capacity(1024, input.as_slice());
        let mut line = Vec::new();
        assert!(read_bounded_line(&mut reader, &mut line, MAX_NDJSON_LINE_BYTES).is_err());
        assert!(line.len() <= MAX_NDJSON_LINE_BYTES);
    }

    #[test]
    fn pairing_slot_stays_exclusive_for_the_full_worker_lifetime() {
        let state = Arc::new(LocalControlState {
            bootstrap_tokens: Mutex::new(Vec::new()),
            sessions: Mutex::new(Vec::new()),
            operations: Mutex::new(Vec::new()),
            active_pairings: AtomicUsize::new(0),
            active_service_operations: AtomicUsize::new(0),
            preferences_path: PathBuf::from("unused-test-preferences.json"),
        });
        let first = claim_pairing_slot(&state).unwrap();
        let (release, released) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            released.recv().unwrap();
            drop(first);
        });
        assert!(claim_pairing_slot(&state).is_err());
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(claim_pairing_slot(&state).is_ok());
    }
}
