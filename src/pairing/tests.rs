#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        path::PathBuf,
        sync::mpsc,
        thread,
    };

    use super::*;
    const MAX_PAIRING_STATE_BYTES: usize = StateFile::Pairing.max_bytes();
    #[cfg(unix)]
    const MAX_ACTIVE_BINDING_BYTES: usize = StateFile::Binding.max_bytes();
    #[cfg(unix)]
    const MAX_AUTH_STATE_BYTES: usize = StateFile::Authorization.max_bytes();

    // Deliberately incomplete authorization fixture for malformed-state tests.
    // Production has no unconditional authorize/invalidate entry points.
    fn persist_auth_state(config: &ClientConfig, state: &LocalAuthState) -> anyhow::Result<()> {
        let transaction = lock_state(config)?;
        persist_auth_state_unlocked(&transaction, state)
    }

    fn write_private_fixture(
        path: impl AsRef<std::path::Path>,
        bytes: impl AsRef<[u8]>,
    ) -> std::io::Result<()> {
        let path = path.as_ref();
        std::fs::write(path, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    fn test_config(directory: PathBuf) -> ClientConfig {
        let config_path = directory.join("config.json");
        ClientConfig {
            endpoint: "https://host-monitoring.example/api/v2/host-monitor/report".into(),
            config_path: Some(config_path),
            state_dir: directory,
            ..ClientConfig::default()
        }
    }

    fn test_host() -> HostIdentity {
        HostIdentity {
            id: Uuid::new_v4().to_string(),
            os: "test".into(),
            os_version: None,
            kernel_version: None,
            arch: "test".into(),
            client_version: "test".into(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn activating_commit_and_reporter_snapshot_stay_under_the_locked_directory() {
        let base = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!("host-state-rebound-{}", Uuid::new_v4()));
        fs::create_dir(&base).unwrap();
        let config = test_config(base.join("state"));
        let transaction = lock_state(&config).unwrap();
        let reader = StateReader::open(&config.state_dir).unwrap();
        let instance_id = Uuid::new_v4();
        let activating = StoredPairingState::Activating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            instance_id,
            activation_url: "https://host-monitoring.example/activate".into(),
            expires_at: Utc::now() + TimeDelta::minutes(10),
            poll_interval: 2,
            pairing_endpoint: config.pairing_endpoint(),
            report_endpoint: config.endpoint.clone(),
            bearer_secret: std::sync::Arc::new(sarmg_client_secret::SecretString::new(
                "original-private-credential".into(),
            )),
        };
        persist_state_unlocked(&transaction, &activating).unwrap();
        fs::rename(&config.state_dir, base.join("held")).unwrap();
        let replacement = StateTransaction::begin(&config.state_dir).unwrap();
        for file in [
            StateFile::Identity,
            StateFile::Credential,
            StateFile::Pairing,
            StateFile::Authorization,
            StateFile::Binding,
        ] {
            replacement
                .write(file, "replacement-must-not-be-touched")
                .unwrap();
        }
        finish_activating_unlocked(&config, &transaction, activating).unwrap();
        assert!(
            matches!(load_state(&transaction).unwrap(), Some(StoredPairingState::Active { instance_id: id, .. }) if id == instance_id)
        );
        // A reader opened before the rename shares the same anchored namespace.
        assert!(matches!(
            load_state(&reader).unwrap(),
            Some(StoredPairingState::Active { .. })
        ));
        let binding = load_active_binding(&config, &transaction).unwrap().unwrap();
        assert!(
            reporter_for_active_binding_unlocked(&config, &transaction, &binding)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            crate::transport::read_secret(&transaction, "test credential")
                .unwrap()
                .expose(),
            "original-private-credential"
        );
        for file in [
            StateFile::Identity,
            StateFile::Credential,
            StateFile::Pairing,
            StateFile::Authorization,
            StateFile::Binding,
        ] {
            assert_eq!(
                replacement.read(file).unwrap(),
                b"replacement-must-not-be-touched"
            );
        }
        drop(replacement);
        drop(reader);
        drop(transaction);
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn local_state_readers_reject_oversized_and_linked_files_without_mutating_state() {
        use std::os::unix::fs::symlink;
        let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!("host-private-read-{}", Uuid::new_v4()));
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        let config = test_config(directory.clone());
        type Reader = fn(&ClientConfig) -> anyhow::Result<()>;
        let cases: [(&str, usize, serde_json::Value, Reader); 3] = [
            (
                PAIRING_STATE_FILE,
                MAX_PAIRING_STATE_BYTES,
                serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"), "phase": "active", "generation": Uuid::new_v4(),
                    "request_id": Uuid::new_v4(), "instance_id": Uuid::new_v4(),
                    "activation_url": "https://host-monitoring.example/activate", "report_endpoint": config.endpoint,
                    "completed_at": Utc::now()
                }),
                |config| {
                    load_state(&StateReader::open(&config.state_dir)?)
                        .map(|value| assert!(value.is_some()))
                },
            ),
            (
                ACTIVE_BINDING_FILE,
                MAX_ACTIVE_BINDING_BYTES,
                serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"), "generation": Uuid::new_v4(),
                    "request_id": Uuid::new_v4(), "instance_id": Uuid::new_v4(), "report_endpoint": config.endpoint
                }),
                |config| {
                    load_active_binding(config, &StateReader::open(&config.state_dir)?)
                        .map(|value| assert!(value.is_some()))
                },
            ),
            (
                AUTH_STATE_FILE,
                MAX_AUTH_STATE_BYTES,
                serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"), "status": "authorized", "reason": "paired", "changed_at": Utc::now()
                }),
                |config| local_auth_state(config).map(|value| assert!(value.is_some())),
            ),
        ];
        for (name, max_bytes, value, read) in cases {
            let path = directory.join(name);
            let mut bytes = serde_json::to_vec(&value).unwrap();
            bytes.resize(max_bytes, b' ');
            write_private_fixture(&path, &bytes).unwrap();
            read(&config).unwrap();
            bytes.push(b' ');
            write_private_fixture(&path, &bytes).unwrap();
            assert!(format!("{:#}", read(&config).unwrap_err()).contains("budget"));
            assert_eq!(fs::read(&path).unwrap(), bytes);
            fs::remove_file(&path).unwrap();
            let victim = directory.join("victim");
            write_private_fixture(&victim, serde_json::to_vec(&value).unwrap()).unwrap();
            symlink(&victim, &path).unwrap();
            assert!(read(&config).is_err());
            fs::remove_file(&path).unwrap();
            fs::hard_link(&victim, &path).unwrap();
            assert!(read(&config).is_err());
            fs::remove_file(&path).unwrap();
            fs::remove_file(victim).unwrap();
        }
        assert_eq!(
            fs::read_dir(&directory).unwrap().count(),
            0,
            "readers must not create locks or state files"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pairing_status_url_appends_path_segments_without_query_or_fragment_ambiguity() {
        let request_id = Uuid::new_v4();
        let endpoint = pairing_status_endpoint(
            "https://host-monitoring.example/api/v2/host-monitor/pairing-requests/",
            request_id,
        )
        .unwrap();
        assert_eq!(
            endpoint.as_str(),
            format!(
                "https://host-monitoring.example/api/v2/host-monitor/pairing-requests/{request_id}/status"
            )
        );

        for invalid in [
            "https://host-monitoring.example/api/v2/host-monitor/pairing-requests?tenant=one",
            "https://host-monitoring.example/api/v2/host-monitor/pairing-requests#bootstrap",
        ] {
            assert!(pairing_status_endpoint(invalid, request_id).is_err());
        }
    }

    #[test]
    fn persisted_pairing_endpoints_are_revalidated_before_network_use() {
        let request_id = Uuid::new_v4();
        let remote_plaintext = "http://192.0.2.10/api/v2/host-monitor/pairing-requests";
        assert!(pairing_status_endpoint(remote_plaintext, request_id).is_err());
        assert!(activation_endpoint(remote_plaintext).is_err());
    }

    #[tokio::test]
    async fn persisted_creating_state_cannot_reuse_remote_plaintext_endpoint() {
        let state = StoredPairingState::Creating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            pairing_endpoint: "http://192.0.2.10/api/v2/host-monitor/pairing-requests".into(),
            report_endpoint: "http://192.0.2.10/api/v2/host-monitor/report".into(),
            host: test_host(),
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        let config = ClientConfig {
            ..ClientConfig::default()
        };

        let error = finish_create_request(&config, state)
            .await
            .expect_err("old state must be checked under the current pairing transport policy");
        assert!(format!("{error:#}").contains("browser pairing requires HTTPS"));
    }

    fn one_shot_pairing_server() -> (String, thread::JoinHandle<()>) {
        one_shot_pairing_server_with_activation_url(|request_id| format!("/activate/{request_id}"))
    }

    fn one_shot_pairing_server_with_activation_url(
        activation_url: impl FnOnce(Uuid) -> String + Send + 'static,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let request_id = Uuid::new_v4();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0_u8; 16 * 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(
                std::str::from_utf8(&request[..read])
                    .unwrap()
                    .starts_with("POST /api/v2/host-monitor/pairing-requests ")
            );
            let body = serde_json::to_vec(&serde_json::json!({
                "request_id": request_id,
                "activation_url": activation_url(request_id),
                "expires_in": 600,
                "poll_interval": 1
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{address}"), handle)
    }

    #[tokio::test]
    async fn create_rejects_cross_origin_activation_url_before_showing_or_persisting_it() {
        let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
            "host-monitoring-pairing-untrusted-activation-{}",
            Uuid::new_v4()
        ));
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        let (server, server_thread) = one_shot_pairing_server_with_activation_url(|request_id| {
            format!("https://attacker.example/activate/{request_id}")
        });
        let config = ClientConfig {
            endpoint: format!("{server}/api/v2/host-monitor/report"),
            pairing_endpoint: Some(format!("{server}/api/v2/host-monitor/pairing-requests")),
            state_dir: directory.clone(),
            ..ClientConfig::default()
        };

        let error = start_or_resume(&config, &test_host())
            .await
            .expect_err("an untrusted browser destination must fail during request creation");
        assert!(error.to_string().contains("does not match"));
        assert!(matches!(
            load_state(&StateReader::open(&config.state_dir).unwrap()).unwrap(),
            Some(StoredPairingState::Creating { .. })
        ));

        server_thread.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    fn delayed_active_server(
        instance_id: Uuid,
    ) -> (
        String,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0_u8; 16 * 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(
                std::str::from_utf8(&request[..read])
                    .unwrap()
                    .contains("/status ")
            );
            seen_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            let body = serde_json::to_vec(&serde_json::json!({
                "status": "active",
                "instance_id": instance_id
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{address}"), seen_rx, release_tx, handle)
    }

    fn delayed_activation_server(
        instance_id: Uuid,
    ) -> (
        String,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0_u8; 16 * 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(
                std::str::from_utf8(&request[..read])
                    .unwrap()
                    .starts_with("POST /api/v2/host-monitor/activate ")
            );
            seen_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            let body = serde_json::to_vec(&serde_json::json!({
                "status": "active",
                "instance_id": instance_id
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{address}"), seen_rx, release_tx, handle)
    }

    #[test]
    fn generated_secrets_have_256_bits_and_hash_the_transmitted_form() {
        let secret = random_secret();
        assert_eq!(secret.expose().len(), 64);
        assert!(secret.expose().bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(sha256_hex(&secret).len(), 64);
        assert_ne!(secret.expose(), sha256_hex(&secret));
    }

    #[test]
    fn polling_authorization_is_explicit_sensitive_and_redacts_errors() {
        use sarmg_client_secret::SecretString;
        let secret = SecretString::new("private-polling-credential".into());
        let header = pairing_authorization(&secret).unwrap();
        assert_eq!(
            header.to_str().unwrap(),
            "Pairing private-polling-credential"
        );
        assert!(header.is_sensitive());
        assert!(!format!("{header:?}/{secret:?}/{secret}").contains(secret.expose()));
        let bad = SecretString::new("private-polling-credential\r\nextra".into());
        let error = pairing_authorization(&bad).unwrap_err();
        assert!(!format!("{error:#}").contains("private-polling-credential"));
    }

    #[test]
    fn journal_secret_snapshots_share_ownership_and_serialization_is_bounded() {
        let directory =
            std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!("host-secret-journal-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        let secret = random_secret();
        let mut state = StoredPairingState::Creating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            pairing_endpoint: config.pairing_endpoint(),
            report_endpoint: config.endpoint.clone(),
            host: test_host(),
            bearer_secret: secret.clone(),
            polling_secret: random_secret(),
        };
        let StoredPairingState::Creating {
            bearer_secret: cloned,
            ..
        } = state.clone()
        else {
            unreachable!()
        };
        assert!(std::sync::Arc::ptr_eq(&secret, &cloned));
        assert!(!format!("{cloned:?}").contains(secret.expose()));
        persist_state(&config, &state).unwrap();
        let original = fs::read(state_path(&config)).unwrap();
        // Only the explicitly opted-in private journal exposes the plaintext.
        assert!(
            std::str::from_utf8(&original)
                .unwrap()
                .contains(secret.expose())
        );
        if let StoredPairingState::Creating {
            report_endpoint, ..
        } = &mut state
        {
            *report_endpoint = "x".repeat(MAX_PAIRING_STATE_BYTES);
        }
        assert!(persist_state(&config, &state).is_err());
        assert_eq!(fs::read(state_path(&config)).unwrap(), original);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2); // stable lock + journal
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn malformed_private_journal_does_not_quote_secret_in_error_chain() {
        let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!("host-secret-errors-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        let transaction = lock_state(&config).unwrap();
        let malformed = format!(
            r#"{{"phase":"private-secret-as-invalid-phase","version":"{}"}}"#,
            env!("CARGO_PKG_VERSION")
        );
        transaction.write(StateFile::Pairing, &malformed).unwrap();
        let error = load_state(&transaction)
            .err()
            .expect("invalid journal must fail");
        assert!(!format!("{error:#}").contains("private-secret"));
        assert!(!format!("{error:?}").contains("private-secret"));
        assert_eq!(fs::read_to_string(state_path(&config)).unwrap(), malformed);
        drop(transaction);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn create_request_contract_contains_hashes_but_not_raw_secrets() {
        let host = HostIdentity {
            id: Uuid::new_v4().to_string(),
            os: "test".into(),
            os_version: None,
            kernel_version: None,
            arch: "test".into(),
            client_version: "test".into(),
        };
        let bearer_secret = random_secret();
        let polling_secret = random_secret();
        let value = serde_json::to_value(CreatePairingRequest {
            host,
            token_hash: sha256_hex(&bearer_secret),
            polling_secret_hash: sha256_hex(&polling_secret),
        })
        .unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 3);
        assert!(object.contains_key("host"));
        assert_eq!(object["token_hash"], sha256_hex(&bearer_secret));
        assert_eq!(object["polling_secret_hash"], sha256_hex(&polling_secret));
        let serialized = serde_json::to_string(&value).unwrap();
        assert!(!serialized.contains(bearer_secret.expose()));
        assert!(!serialized.contains(polling_secret.expose()));
    }

    #[test]
    fn status_contract_accepts_only_current_waiting_value() {
        let response: PairingStatusResponse = serde_json::from_value(serde_json::json!({
            "status": "waiting"
        }))
        .unwrap();
        assert!(matches!(response.status, PairingStatus::Waiting));
        assert!(response.instance_id.is_none());
        assert!(
            serde_json::from_value::<PairingStatusResponse>(serde_json::json!({
                "status": "pending"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PairingStatusResponse>(serde_json::json!({
                "status": "waiting",
                "pending": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PairingStatusResponse>(serde_json::json!({
                "status": "active",
                "instance_id": Uuid::new_v4().to_string().to_uppercase()
            }))
            .is_err()
        );
    }

    #[test]
    fn current_pairing_responses_and_local_auth_state_reject_unknown_fields() {
        assert!(
            serde_json::from_value::<CreatePairingResponse>(serde_json::json!({
                "request_id": Uuid::new_v4(),
                "activation_url": "https://host-monitoring.example/client/activate/request",
                "expires_in": 300,
                "poll_interval": 2,
                "enrollment_secret": "unexpected"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<CreatePairingResponse>(serde_json::json!({
                "request_id": Uuid::new_v4().to_string().replace('-', ""),
                "activation_url": "https://host-monitoring.example/client/activate/request",
                "expires_in": 300,
                "poll_interval": 2
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivatePairingResponse>(serde_json::json!({
                "instance_id": Uuid::new_v4(),
                "status": "active",
                "token": "unexpected"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivatePairingResponse>(serde_json::json!({
                "instance_id": Uuid::new_v4(),
                "status": "pending"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivatePairingResponse>(serde_json::json!({
                "instance_id": Uuid::new_v4().to_string().to_uppercase(),
                "status": "active"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<LocalAuthState>(serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "status": "authorized",
                "reason": "browser pairing completed",
                "changed_at": Utc::now(),
                "unknown_extension": true
            }))
            .is_err()
        );
        for version in [
            serde_json::Value::Null,
            serde_json::json!(1),
            serde_json::json!("0.0.0"),
        ] {
            let mut state = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "status": "authorized",
                "reason": "browser pairing completed",
                "changed_at": Utc::now()
            });
            if version.is_null() {
                state.as_object_mut().unwrap().remove("version");
            } else {
                state["version"] = version;
            }
            assert!(serde_json::from_value::<LocalAuthState>(state).is_err());
        }
    }

    #[test]
    fn non_json_success_points_to_the_server_origin_without_leaking_the_body() {
        let endpoint = "http://127.0.0.1/api/v2/host-monitor/pairing-requests";
        let body = b"<!doctype html><title>POETIZE private marker</title>";
        let error = parse_pairing_json::<CreatePairingResponse>(
            body,
            "text/html; charset=utf-8",
            endpoint,
            "pairing response",
        )
        .expect_err("HTML must not be accepted as a pairing response");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("Server origin http://127.0.0.1"));
        assert!(!rendered.contains("/api/v2/host-monitor/pairing-requests"));
        assert!(rendered.contains("Content-Type: text/html"));
        assert!(rendered.contains("address or port may be wrong"));
        assert!(rendered.contains("including its port"));
        assert!(!rendered.contains("POETIZE"));
        assert!(!rendered.contains("private marker"));

        let valid_json = serde_json::to_vec(&serde_json::json!({
            "request_id": Uuid::new_v4(),
            "activation_url": "/client/activate/request",
            "expires_in": 600,
            "poll_interval": 2
        }))
        .unwrap();
        assert!(
            parse_pairing_json::<CreatePairingResponse>(
                &valid_json,
                "text/plain",
                endpoint,
                "pairing response"
            )
            .is_err()
        );
        assert!(
            parse_pairing_json::<CreatePairingResponse>(
                &valid_json,
                "application/vnd.host-monitoring+json",
                endpoint,
                "pairing response"
            )
            .is_err()
        );
    }

    #[test]
    fn pairing_operations_accept_only_their_current_http_statuses() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::BAD_GATEWAY,
        ] {
            let response = sarmg_client_secure_http::BoundedResponse {
                status,
                headers: Default::default(),
                body: b"reflected-pairing-secret".to_vec(),
            };
            let error =
                ensure_pairing_status(response.status, &[StatusCode::OK], "poll pairing status")
                    .unwrap_err();
            assert!(!format!("{error:#}/{error:?}").contains("reflected-pairing-secret"));
        }
        assert!(
            ensure_pairing_status(
                StatusCode::OK,
                &[StatusCode::OK, StatusCode::CREATED],
                "create pairing request"
            )
            .is_ok()
        );
        assert!(
            ensure_pairing_status(
                StatusCode::CREATED,
                &[StatusCode::OK, StatusCode::CREATED],
                "create pairing request"
            )
            .is_ok()
        );
        for operation in [
            "poll pairing status",
            "submit the one-time authorization key",
        ] {
            assert!(ensure_pairing_status(StatusCode::OK, &[StatusCode::OK], operation).is_ok());
            assert!(
                ensure_pairing_status(StatusCode::NO_CONTENT, &[StatusCode::OK], operation)
                    .is_err()
            );
        }
    }

    #[test]
    fn malformed_json_source_and_endpoint_secrets_are_fully_redacted() {
        let marker = "uci_SECRET_MARKER_MUST_NOT_LEAK";
        let body = format!(r#"{{"status":"{marker}"}}"#);
        let endpoint = format!(
            "https://user:{marker}@host-monitoring.example/api/v2/host-monitor/pairing-requests?key={marker}#{marker}"
        );
        let error = parse_pairing_json::<PairingStatusResponse>(
            body.as_bytes(),
            "application/json",
            &endpoint,
            "pairing status response",
        )
        .expect_err("an unknown status must not be accepted");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("Server origin https://host-monitoring.example"));
        assert!(rendered.contains("Content-Type: application/json"));
        assert!(!rendered.contains(marker));
        assert!(!rendered.contains("unknown variant"));
        assert!(!rendered.contains("Caused by"));
    }

    #[test]
    fn diagnostic_content_type_does_not_echo_parameters_or_unknown_values() {
        let marker = "uci_SECRET_MARKER_MUST_NOT_LEAK";
        assert_eq!(
            pairing_content_type_for_diagnostics(&format!("text/html; reflected={marker}")),
            "text/html"
        );
        assert_eq!(
            pairing_content_type_for_diagnostics(&format!("application/{marker}")),
            "<unexpected>"
        );
    }

    #[test]
    fn relative_activation_url_is_resolved_to_the_console_origin() {
        assert_eq!(
            resolve_activation_url(
                "https://host-monitoring.example/api/v2/host-monitor/pairing-requests",
                "/activate/00000000-0000-4000-8000-000000000001",
            )
            .unwrap(),
            "https://host-monitoring.example/activate/00000000-0000-4000-8000-000000000001"
        );
    }

    #[test]
    fn insecure_override_never_applies_to_remote_activation_pages() {
        assert!(
            resolve_activation_url(
                "http://192.0.2.10/api/v2/host-monitor/pairing-requests",
                "/activate/00000000-0000-4000-8000-000000000001",
            )
            .is_err()
        );
        assert!(
            resolve_activation_url(
                "http://127.0.0.1:8081/api/v2/host-monitor/pairing-requests",
                "/activate/00000000-0000-4000-8000-000000000001",
            )
            .is_ok()
        );
        assert!(
            resolve_activation_url(
                "http://[::1]:8081/api/v2/host-monitor/pairing-requests",
                "/activate/00000000-0000-4000-8000-000000000001",
            )
            .is_ok()
        );
    }

    #[test]
    fn activation_endpoint_and_public_url_stay_bound_to_the_pairing_origin() {
        let request_id = Uuid::new_v4();
        assert_eq!(
            activation_endpoint(
                "https://host-monitoring.example/prefix/api/v2/host-monitor/pairing-requests"
            )
            .unwrap()
            .as_str(),
            "https://host-monitoring.example/prefix/api/v2/host-monitor/activate"
        );
        validate_activation_url_request(
            &format!("https://host-monitoring.example/activate/{request_id}"),
            "https://host-monitoring.example/prefix/api/v2/host-monitor/pairing-requests",
            request_id,
        )
        .unwrap();
        assert!(
            validate_activation_url_request(
                &format!("https://attacker.example/activate/{request_id}"),
                "https://host-monitoring.example/api/v2/host-monitor/pairing-requests",
                request_id,
            )
            .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn service_activation_commit_wins_the_post_response_race_idempotently() {
        let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
            "host-monitoring-activation-race-{}",
            Uuid::new_v4()
        ));
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        let instance_id = Uuid::new_v4();
        let (server, request_seen, release_response, server_thread) =
            delayed_activation_server(instance_id);
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let config = ClientConfig {
            endpoint: format!("{server}/api/v2/host-monitor/report"),
            pairing_endpoint: Some(format!("{server}/api/v2/host-monitor/pairing-requests")),
            state_dir: directory.clone(),
            ..ClientConfig::default()
        };
        persist_state(
            &config,
            &StoredPairingState::Pending {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: format!("{server}/activate/{request_id}"),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 1,
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: random_secret(),
                polling_secret: random_secret(),
            },
        )
        .unwrap();

        let activation_config = config.clone();
        let activation = tokio::spawn(async move {
            activate_pending_with_code(
                &activation_config,
                generation,
                request_id,
                "uci_test_authorization_key",
            )
            .await
        });
        request_seen
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        persist_state(
            &config,
            &StoredPairingState::Active {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: format!("{server}/activate/{request_id}"),
                instance_id,
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        release_response.send(()).unwrap();
        assert_eq!(activation.await.unwrap().unwrap(), Some(instance_id));
        server_thread.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pending_state_round_trips_privately() {
        let directory =
            std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!("host-monitoring-pairing-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        let state = StoredPairingState::Pending {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            activation_url: "https://host-monitoring.example/client/activate/test".into(),
            expires_at: Utc::now(),
            poll_interval: 5,
            pairing_endpoint: config.pairing_endpoint(),
            report_endpoint: config.endpoint.clone(),
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        persist_state(&config, &state).unwrap();
        assert!(matches!(
            load_state(&StateReader::open(&config.state_dir).unwrap()).unwrap(),
            Some(StoredPairingState::Pending { .. })
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(state_path(&config))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn creating_state_round_trips_the_same_secrets_for_idempotent_retry() {
        let directory =
            std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!("host-monitoring-creating-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        let bearer_secret = random_secret();
        let polling_secret = random_secret();
        let state = StoredPairingState::Creating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            pairing_endpoint: config.pairing_endpoint(),
            report_endpoint: config.endpoint.clone(),
            host: HostIdentity {
                id: Uuid::new_v4().to_string(),
                os: "test".into(),
                os_version: None,
                kernel_version: None,
                arch: "test".into(),
                client_version: "test".into(),
            },
            bearer_secret: bearer_secret.clone(),
            polling_secret: polling_secret.clone(),
        };
        let mut encoded = serde_json::to_value(&state).unwrap();
        assert_eq!(encoded["version"], env!("CARGO_PKG_VERSION"));
        encoded["version"] = serde_json::json!(1);
        assert!(serde_json::from_value::<StoredPairingState>(encoded).is_err());
        persist_state(&config, &state).unwrap();
        assert!(matches!(
            load_state(&StateReader::open(&config.state_dir).unwrap()).unwrap(),
            Some(StoredPairingState::Creating {
                bearer_secret: saved_bearer,
                polling_secret: saved_polling,
                ..
            }) if saved_bearer.expose() == bearer_secret.expose() && saved_polling.expose() == polling_secret.expose()
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn live_pending_request_cannot_be_silently_moved_to_another_server() {
        let directory =
            std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!("host-monitoring-pending-origin-{}", Uuid::new_v4()));
        let mut config = test_config(directory.clone());
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        let config_path = directory.join("config.json");
        config.config_path = Some(config_path.clone());
        let old_config = serde_json::to_vec(&config).unwrap();
        write_private_fixture(&config_path, &old_config).unwrap();
        write_private_fixture(directory.join("client-token"), "existing-long-lived-token").unwrap();
        let state = StoredPairingState::Pending {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            activation_url: "https://old.example/client/activate/test".into(),
            expires_at: Utc::now() + TimeDelta::minutes(10),
            poll_interval: 5,
            pairing_endpoint: "https://old.example/api/v2/host-monitor/pairing-requests".into(),
            report_endpoint: "https://old.example/api/v2/host-monitor/report".into(),
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        persist_state(&config, &state).unwrap();
        config.endpoint = "https://new.example/api/v2/host-monitor/report".into();
        let error = start_or_resume(&config, &test_host())
            .await
            .expect_err("a live request must stay bound to its original server");
        assert!(
            error
                .to_string()
                .contains("different Host Monitoring server")
        );
        assert!(matches!(
            load_state(&StateReader::open(&config.state_dir).unwrap()).unwrap(),
            Some(StoredPairingState::Pending { pairing_endpoint, .. })
                if pairing_endpoint.starts_with("https://old.example/")
        ));
        assert_eq!(fs::read(&config_path).unwrap(), old_config);
        assert_eq!(
            fs::read_to_string(directory.join("client-token")).unwrap(),
            "existing-long-lived-token"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn interrupted_create_cannot_be_silently_moved_to_another_server() {
        let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
            "host-monitoring-creating-origin-{}",
            Uuid::new_v4()
        ));
        let mut config = test_config(directory.clone());
        let state = StoredPairingState::Creating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            pairing_endpoint: "https://old.example/api/v2/host-monitor/pairing-requests".into(),
            report_endpoint: "https://old.example/api/v2/host-monitor/report".into(),
            host: test_host(),
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        persist_state(&config, &state).unwrap();
        config.endpoint = "https://new.example/api/v2/host-monitor/report".into();
        let error = start_or_resume(&config, &test_host())
            .await
            .expect_err("an interrupted create must stay bound to its original server");
        assert!(
            error
                .to_string()
                .contains("different Host Monitoring server")
        );
        assert!(matches!(
            load_state(&StateReader::open(&config.state_dir).unwrap()).unwrap(),
            Some(StoredPairingState::Creating { pairing_endpoint, .. })
                if pairing_endpoint.starts_with("https://old.example/")
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn explicit_replacement_rotates_same_origin_incomplete_state() {
        for phase in ["creating", "expired_pending"] {
            let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
                "host-monitoring-same-origin-replace-{phase}-{}",
                Uuid::new_v4()
            ));
            let mut config = test_config(directory.clone());
            let old_generation = Uuid::new_v4();
            let old_bearer = random_secret();
            let old_polling = random_secret();
            let state = if phase == "creating" {
                StoredPairingState::Creating {
                    version: PAIRING_STATE_VERSION,
                    generation: old_generation,
                    pairing_endpoint: config.pairing_endpoint(),
                    report_endpoint: config.endpoint.clone(),
                    host: test_host(),
                    bearer_secret: old_bearer.clone(),
                    polling_secret: old_polling.clone(),
                }
            } else {
                let request_id = Uuid::new_v4();
                StoredPairingState::Pending {
                    version: PAIRING_STATE_VERSION,
                    generation: old_generation,
                    request_id,
                    activation_url: format!(
                        "https://host-monitoring.example/client/activate/{request_id}"
                    ),
                    expires_at: Utc::now() - TimeDelta::minutes(1),
                    poll_interval: 5,
                    pairing_endpoint: config.pairing_endpoint(),
                    report_endpoint: config.endpoint.clone(),
                    bearer_secret: old_bearer.clone(),
                    polling_secret: old_polling.clone(),
                }
            };
            persist_state(&config, &state).unwrap();

            match (phase, prepare_start(&config, &test_host()).unwrap()) {
                ("creating", PairingStart::Create(resumed)) => match *resumed {
                    StoredPairingState::Creating {
                        generation,
                        bearer_secret,
                        polling_secret,
                        ..
                    } => {
                        assert_eq!(generation, old_generation);
                        assert_eq!(bearer_secret.expose(), old_bearer.expose());
                        assert_eq!(polling_secret.expose(), old_polling.expose());
                    }
                    _ => panic!("creating state was not resumed"),
                },
                ("expired_pending", PairingStart::Waiting(session)) => {
                    assert_eq!(session.generation, old_generation);
                }
                _ => panic!("ordinary pairing did not conservatively resume saved state"),
            }

            config.replace_pending_pairing = true;
            let PairingStart::Create(replacement) = prepare_start(&config, &test_host()).unwrap()
            else {
                panic!("explicit replacement did not create a fresh generation");
            };
            let StoredPairingState::Creating {
                generation: new_generation,
                bearer_secret: new_bearer,
                polling_secret: new_polling,
                ..
            } = *replacement
            else {
                panic!("explicit replacement did not persist a creating state");
            };
            assert_ne!(new_generation, old_generation);
            assert_ne!(new_bearer.expose(), old_bearer.expose());
            assert_ne!(new_polling.expose(), old_polling.expose());
            assert!(matches!(
                load_state(&StateReader::open(&config.state_dir).unwrap()).unwrap(),
                Some(StoredPairingState::Creating {
                    generation,
                    bearer_secret,
                    polling_secret,
                    ..
                }) if generation == new_generation
                    && bearer_secret.expose() == new_bearer.expose()
                    && polling_secret.expose() == new_polling.expose()
            ));
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test]
    async fn confirmed_tray_replacement_can_replace_mismatched_incomplete_states() {
        for old_state in ["creating", "pending"] {
            let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
                "host-monitoring-confirmed-replace-{old_state}-{}",
                Uuid::new_v4()
            ));
            let (server, server_thread) = one_shot_pairing_server();
            let mut config = ClientConfig {
                endpoint: format!("{server}/api/v2/host-monitor/report"),
                state_dir: directory.clone(),
                replace_pending_pairing: true,
                ..ClientConfig::default()
            };
            config.pairing_endpoint =
                Some(format!("{server}/api/v2/host-monitor/pairing-requests"));
            let state = if old_state == "creating" {
                StoredPairingState::Creating {
                    version: PAIRING_STATE_VERSION,
                    generation: Uuid::new_v4(),
                    pairing_endpoint: "https://old.example/api/v2/host-monitor/pairing-requests"
                        .into(),
                    report_endpoint: "https://old.example/api/v2/host-monitor/report".into(),
                    host: test_host(),
                    bearer_secret: random_secret(),
                    polling_secret: random_secret(),
                }
            } else {
                StoredPairingState::Pending {
                    version: PAIRING_STATE_VERSION,
                    generation: Uuid::new_v4(),
                    request_id: Uuid::new_v4(),
                    activation_url: "https://old.example/client/activate/test".into(),
                    expires_at: Utc::now() + TimeDelta::minutes(10),
                    poll_interval: 5,
                    pairing_endpoint: "https://old.example/api/v2/host-monitor/pairing-requests"
                        .into(),
                    report_endpoint: "https://old.example/api/v2/host-monitor/report".into(),
                    bearer_secret: random_secret(),
                    polling_secret: random_secret(),
                }
            };
            persist_state(&config, &state).unwrap();
            let session = start_or_resume(&config, &test_host())
                .await
                .expect("the explicitly confirmed new origin should replace incomplete state");
            assert!(session.activation_url.starts_with(&server));
            assert!(matches!(
                load_state(&StateReader::open(&config.state_dir).unwrap()).unwrap(),
                Some(StoredPairingState::Pending { pairing_endpoint, .. })
                    if pairing_endpoint == format!("{server}/api/v2/host-monitor/pairing-requests")
            ));
            server_thread.join().unwrap();
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delayed_old_activation_cannot_overwrite_a_replacement_generation() {
        let directory =
            std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!("host-monitoring-delayed-active-{}", Uuid::new_v4()));
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        let old_instance_id = Uuid::new_v4();
        let (old_server, request_seen, release_response, old_thread) =
            delayed_active_server(old_instance_id);
        let old_config_path = directory.join("config.json");
        let old_pairing_endpoint = format!("{old_server}/api/v2/host-monitor/pairing-requests");
        let old_report_endpoint = format!("{old_server}/api/v2/host-monitor/report");
        let old_config = ClientConfig {
            endpoint: old_report_endpoint.clone(),
            pairing_endpoint: Some(old_pairing_endpoint.clone()),
            state_dir: directory.clone(),
            config_path: Some(old_config_path.clone()),
            ..ClientConfig::default()
        };
        let old_config_bytes = serde_json::to_vec(&old_config).unwrap();
        write_private_fixture(&old_config_path, &old_config_bytes).unwrap();
        write_private_fixture(directory.join("client-token"), "old-long-lived-token").unwrap();
        let old_host_id = Uuid::new_v4();
        write_private_fixture(directory.join("host-id"), old_host_id.to_string()).unwrap();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let old_state = StoredPairingState::Pending {
            version: PAIRING_STATE_VERSION,
            generation,
            request_id,
            activation_url: format!("{old_server}/client/activate/{request_id}"),
            expires_at: Utc::now() + TimeDelta::minutes(10),
            poll_interval: 1,
            pairing_endpoint: old_pairing_endpoint.clone(),
            report_endpoint: old_report_endpoint.clone(),
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        persist_state(&old_config, &old_state).unwrap();
        let polling_config = old_config.clone();
        let stale_poll = tokio::spawn(async move { poll_existing(&polling_config).await });
        request_seen
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();

        let (new_server, new_thread) = one_shot_pairing_server();
        let new_config = ClientConfig {
            endpoint: format!("{new_server}/api/v2/host-monitor/report"),
            pairing_endpoint: Some(format!("{new_server}/api/v2/host-monitor/pairing-requests")),
            state_dir: directory.clone(),
            config_path: Some(old_config_path.clone()),
            replace_pending_pairing: true,
            ..ClientConfig::default()
        };
        let new_session = start_or_resume(&new_config, &test_host()).await.unwrap();
        release_response.send(()).unwrap();
        let stale_error = stale_poll
            .await
            .unwrap()
            .expect_err("the delayed old Active response must lose its generation CAS");
        assert!(stale_error.is::<PairingSuperseded>());
        assert!(matches!(
            load_state(&StateReader::open(&new_config.state_dir).unwrap()).unwrap(),
            Some(StoredPairingState::Pending {
                generation: saved_generation,
                pairing_endpoint,
                ..
            }) if saved_generation == new_session.generation
                && pairing_endpoint.starts_with(&new_server)
        ));
        assert_eq!(
            fs::read_to_string(directory.join("client-token")).unwrap(),
            "old-long-lived-token"
        );
        assert_eq!(
            fs::read_to_string(directory.join("host-id")).unwrap(),
            old_host_id.to_string()
        );
        assert_eq!(fs::read(&old_config_path).unwrap(), old_config_bytes);
        old_thread.join().unwrap();
        new_thread.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn activating_journal_recovers_all_endpoint_bound_files() {
        for preexisting in [false, true] {
            let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
                "host-monitoring-activating-recovery-{preexisting}-{}",
                Uuid::new_v4()
            ));
            crate::private_fs::ensure_private_directory(&directory).unwrap();
            // Model an administrator-owned system config that the service cannot replace. A
            // directory is deterministic even when this test happens to run as root.
            let config_path = directory.join("operator-config");
            fs::create_dir(&config_path).unwrap();
            let mut config = ClientConfig {
                endpoint: "https://old.example/api/v2/host-monitor/report".into(),
                state_dir: directory.clone(),
                config_path: Some(config_path.clone()),
                ..ClientConfig::default()
            };
            if preexisting {
                write_private_fixture(directory.join("client-token"), "old-token").unwrap();
                write_private_fixture(directory.join("host-id"), Uuid::new_v4().to_string())
                    .unwrap();
            }
            let generation = Uuid::new_v4();
            let request_id = Uuid::new_v4();
            let instance_id = Uuid::new_v4();
            let new_token = random_secret();
            persist_state(
                &config,
                &StoredPairingState::Activating {
                    version: PAIRING_STATE_VERSION,
                    generation,
                    request_id,
                    activation_url: "https://new.example/client/activate/test".into(),
                    expires_at: Utc::now() + TimeDelta::minutes(10),
                    poll_interval: 1,
                    instance_id,
                    pairing_endpoint: "https://new.example/api/v2/host-monitor/pairing-requests"
                        .into(),
                    report_endpoint: "https://new.example/api/v2/host-monitor/report".into(),
                    bearer_secret: new_token.clone(),
                },
            )
            .unwrap();

            let progress = poll_existing(&config).await.unwrap().unwrap();
            assert!(matches!(
                progress,
                PairingProgress::Active {
                    generation: saved_generation,
                    request_id: saved_request,
                    instance_id: saved_instance,
                    ..
                } if saved_generation == generation
                    && saved_request == request_id
                    && saved_instance == instance_id
            ));
            assert_eq!(
                fs::read_to_string(directory.join("client-token")).unwrap(),
                new_token.expose()
            );
            assert_eq!(
                fs::read_to_string(directory.join("host-id")).unwrap(),
                instance_id.to_string()
            );
            assert_eq!(
                load_active_binding(&config, &StateReader::open(&config.state_dir).unwrap())
                    .unwrap(),
                Some(ActiveBinding {
                    version: PAIRING_STATE_VERSION,
                    generation,
                    request_id,
                    instance_id,
                    report_endpoint: "https://new.example/api/v2/host-monitor/report".into(),
                })
            );
            let binding_before_status = fs::read(active_binding_path(&config)).unwrap();
            let status = local_status(&config).unwrap();
            assert_eq!(
                status.active_report_endpoint.as_deref(),
                Some("https://new.example/api/v2/host-monitor/report")
            );
            assert_eq!(
                fs::read(active_binding_path(&config)).unwrap(),
                binding_before_status
            );
            assert!(config_path.is_dir());
            assert!(matches!(
                load_state(&StateReader::open(&config.state_dir).unwrap()).unwrap(),
                Some(StoredPairingState::Active {
                    generation: saved_generation,
                    ..
                }) if saved_generation == generation
            ));
            let mut host = test_host();
            activate_reporter_snapshot(
                &mut config,
                &mut host,
                generation,
                request_id,
                instance_id,
                "https://new.example/api/v2/host-monitor/report",
            )
            .unwrap();
            assert_eq!(
                config.endpoint,
                "https://new.example/api/v2/host-monitor/report"
            );
            assert!(config_path.is_dir());
            assert!(matches!(
                poll_existing(&config).await.unwrap(),
                Some(PairingProgress::Active {
                    generation: saved_generation,
                    ..
                }) if saved_generation == generation
            ));
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn active_state_without_binding_is_rejected() {
        let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
            "host-monitoring-missing-binding-{}",
            Uuid::new_v4()
        ));
        let config = test_config(directory.clone());
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        write_private_fixture(directory.join("client-token"), "current-token").unwrap();
        write_private_fixture(directory.join("host-id"), instance_id.to_string()).unwrap();
        persist_auth_state(
            &config,
            &LocalAuthState {
                version: PAIRING_STATE_VERSION,
                status: CredentialAuthorization::Authorized,
                reason: "existing installation".into(),
                changed_at: Utc::now(),
            },
        )
        .unwrap();
        persist_state(
            &config,
            &StoredPairingState::Active {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://host-monitoring.example/client/activate/test".into(),
                instance_id,
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();

        assert!(!active_binding_path(&config).exists());
        assert!(
            local_status(&config)
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
        let error = reporter_for_current_active_state(&config)
            .err()
            .expect("missing binding must fail");
        assert!(error.to_string().contains("missing"));
        assert!(!active_binding_path(&config).exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn mismatched_active_binding_is_never_silently_replaced() {
        let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
            "host-monitoring-binding-mismatch-{}",
            Uuid::new_v4()
        ));
        let mut config = test_config(directory.clone());
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        write_private_fixture(directory.join("client-token"), "current-token").unwrap();
        write_private_fixture(directory.join("host-id"), instance_id.to_string()).unwrap();
        persist_auth_state(
            &config,
            &LocalAuthState {
                version: PAIRING_STATE_VERSION,
                status: CredentialAuthorization::Authorized,
                reason: "test".into(),
                changed_at: Utc::now(),
            },
        )
        .unwrap();
        persist_state(
            &config,
            &StoredPairingState::Active {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://host-monitoring.example/client/activate/test".into(),
                instance_id,
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        let mismatched = ActiveBinding {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            request_id,
            instance_id,
            report_endpoint: config.endpoint.clone(),
        };
        persist_active_binding_unlocked(&config, &lock_state(&config).unwrap(), &mismatched)
            .unwrap();

        let status_error =
            local_status(&config).expect_err("status must reject a mismatched binding");
        assert!(status_error.to_string().contains("does not match"));
        let reporter_error = match reporter_for_current_active_state(&config) {
            Ok(_) => panic!("a mismatched binding must fail closed"),
            Err(error) => error,
        };
        assert!(reporter_error.to_string().contains("does not match"));
        let config_error = commit_active_configuration(
            &mut config,
            generation,
            request_id,
            instance_id,
            "https://host-monitoring.example/api/v2/host-monitor/report",
        )
        .expect_err("config synchronization must not replace a mismatched binding");
        assert!(config_error.to_string().contains("does not match"));
        assert_eq!(
            load_active_binding(&config, &StateReader::open(&config.state_dir).unwrap()).unwrap(),
            Some(mismatched)
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn replacing_current_active_state_preserves_its_endpoint_binding() {
        let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
            "host-monitoring-binding-before-create-{}",
            Uuid::new_v4()
        ));
        let mut config = test_config(directory.clone());
        let old_generation = Uuid::new_v4();
        let old_request_id = Uuid::new_v4();
        let old_instance_id = Uuid::new_v4();
        let old_endpoint = config.endpoint.clone();
        persist_state(
            &config,
            &StoredPairingState::Active {
                version: PAIRING_STATE_VERSION,
                generation: old_generation,
                request_id: old_request_id,
                activation_url: "https://host-monitoring.example/client/activate/old".into(),
                instance_id: old_instance_id,
                report_endpoint: old_endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        persist_active_binding_unlocked(
            &config,
            &lock_state(&config).unwrap(),
            &ActiveBinding {
                version: PAIRING_STATE_VERSION,
                generation: old_generation,
                request_id: old_request_id,
                instance_id: old_instance_id,
                report_endpoint: old_endpoint.clone(),
            },
        )
        .unwrap();
        config.endpoint = "https://new.example/api/v2/host-monitor/report".into();
        config.pairing_endpoint =
            Some("https://new.example/api/v2/host-monitor/pairing-requests".into());

        let PairingStart::Create(creating) = prepare_start(&config, &test_host()).unwrap() else {
            panic!("an Active state must allow a new explicitly requested pairing generation");
        };
        assert!(matches!(
            *creating,
            StoredPairingState::Creating { ref report_endpoint, .. }
                if report_endpoint == "https://new.example/api/v2/host-monitor/report"
        ));
        assert_eq!(
            load_active_binding(&config, &StateReader::open(&config.state_dir).unwrap()).unwrap(),
            Some(ActiveBinding {
                version: PAIRING_STATE_VERSION,
                generation: old_generation,
                request_id: old_request_id,
                instance_id: old_instance_id,
                report_endpoint: old_endpoint,
            })
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn run_keeps_the_current_credential_during_an_incomplete_pairing_attempt() {
        let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
            "host-monitoring-current-reporter-{}",
            Uuid::new_v4()
        ));
        let config = test_config(directory.clone());
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        write_private_fixture(directory.join("client-token"), "current-long-lived-token").unwrap();
        let active_generation = Uuid::new_v4();
        let active_request_id = Uuid::new_v4();
        let active_instance_id = Uuid::new_v4();
        write_private_fixture(directory.join("host-id"), active_instance_id.to_string()).unwrap();
        persist_active_binding_unlocked(
            &config,
            &lock_state(&config).unwrap(),
            &ActiveBinding {
                version: PAIRING_STATE_VERSION,
                generation: active_generation,
                request_id: active_request_id,
                instance_id: active_instance_id,
                report_endpoint: config.endpoint.clone(),
            },
        )
        .unwrap();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let states = [
            StoredPairingState::Creating {
                version: PAIRING_STATE_VERSION,
                generation,
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                host: test_host(),
                bearer_secret: random_secret(),
                polling_secret: random_secret(),
            },
            StoredPairingState::Pending {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://host-monitoring.example/client/activate/test".into(),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 5,
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: random_secret(),
                polling_secret: random_secret(),
            },
        ];
        persist_state(&config, &states[1]).unwrap();
        assert!(
            existing_reporter_for_run(&config).unwrap().is_none(),
            "a token and pairing journal without current authorized state must be rejected"
        );
        persist_auth_state(
            &config,
            &LocalAuthState {
                version: PAIRING_STATE_VERSION,
                status: CredentialAuthorization::Authorized,
                reason: "current pairing completed".into(),
                changed_at: Utc::now(),
            },
        )
        .unwrap();
        for state in states {
            persist_state(&config, &state).unwrap();
            assert!(existing_reporter_for_run(&config).unwrap().is_some());
            assert!(has_current_authorized_identity(&config).unwrap());
        }

        persist_state(
            &config,
            &StoredPairingState::Denied {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://host-monitoring.example/client/activate/test".into(),
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        assert!(
            existing_reporter_for_run(&config).unwrap().is_some(),
            "a denied pairing attempt must not discard the still-authorized credential"
        );
        assert!(has_current_authorized_identity(&config).unwrap());

        persist_state(
            &config,
            &StoredPairingState::Expired {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://host-monitoring.example/client/activate/test".into(),
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        assert!(
            existing_reporter_for_run(&config).unwrap().is_some(),
            "an expired pairing attempt must not discard the still-authorized credential"
        );
        assert!(has_current_authorized_identity(&config).unwrap());

        fs::remove_file(directory.join(PAIRING_STATE_FILE)).unwrap();
        assert!(
            existing_reporter_for_run(&config).unwrap().is_none(),
            "a raw token without current package-version pairing state must be rejected"
        );

        write_private_fixture(directory.join("client-token"), "active-token").unwrap();
        persist_state(
            &config,
            &StoredPairingState::Active {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://host-monitoring.example/client/activate/test".into(),
                instance_id: Uuid::new_v4(),
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        assert!(existing_reporter_for_run(&config).unwrap().is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn local_inspection_does_not_create_a_lock_or_state_directory() {
        let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
            "host-monitoring-read-only-status-{}",
            Uuid::new_v4()
        ));
        let config = test_config(directory.clone());

        assert!(local_progress(&config).unwrap().is_none());
        assert!(local_auth_state(&config).unwrap().is_none());
        assert!(
            !directory.exists(),
            "read-only status inspection must not create the state directory"
        );
    }

    #[test]
    fn local_inspection_does_not_publish_an_activating_credential() {
        let directory = std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!(
            "host-monitoring-read-only-activating-{}",
            Uuid::new_v4()
        ));
        let config = test_config(directory.clone());
        let state = StoredPairingState::Activating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            activation_url: "https://host-monitoring.example/client/activate/test".into(),
            expires_at: Utc::now() + TimeDelta::minutes(10),
            poll_interval: 5,
            instance_id: Uuid::new_v4(),
            pairing_endpoint: config.pairing_endpoint(),
            report_endpoint: config.endpoint.clone(),
            bearer_secret: random_secret(),
        };
        persist_state(&config, &state).unwrap();
        let state_path = state_path(&config);
        let before = fs::read(&state_path).unwrap();

        assert!(matches!(
            local_progress(&config).unwrap(),
            Some(PairingProgress::Creating { .. })
        ));
        assert_eq!(fs::read(&state_path).unwrap(), before);
        assert!(!directory.join("client-token").exists());
        assert!(!directory.join("host-id").exists());
        assert!(!directory.join("auth-state.json").exists());
        assert!(!active_binding_path(&config).exists());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn activation_atomically_commits_server_identity_and_token() {
        let directory =
            std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!("host-monitoring-activation-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        write_private_fixture(directory.join("host-id"), Uuid::new_v4().to_string()).unwrap();
        write_private_fixture(directory.join("client-token"), "old-token").unwrap();
        let instance_id = Uuid::new_v4();
        let bearer_secret = random_secret();
        let polling_secret = random_secret();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let pairing_endpoint = config.pairing_endpoint();
        persist_state(
            &config,
            &StoredPairingState::Pending {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://host-monitoring.example/client/activate/test".into(),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 5,
                pairing_endpoint: pairing_endpoint.clone(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: bearer_secret.clone(),
                polling_secret: polling_secret.clone(),
            },
        )
        .unwrap();

        persist_active_credentials(
            &config,
            load_state(&StateReader::open(&config.state_dir).unwrap())
                .unwrap()
                .unwrap(),
            instance_id,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(directory.join("host-id")).unwrap(),
            instance_id.to_string()
        );
        assert_eq!(
            fs::read_to_string(directory.join("client-token")).unwrap(),
            bearer_secret.expose()
        );
        let binding: ActiveBinding =
            serde_json::from_slice(&fs::read(directory.join(ACTIVE_BINDING_FILE)).unwrap())
                .unwrap();
        assert_eq!(
            binding,
            ActiveBinding {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                instance_id,
                report_endpoint: config.endpoint.clone(),
            }
        );
        assert!(matches!(
            load_state(&StateReader::open(&config.state_dir).unwrap()).unwrap(),
            Some(StoredPairingState::Active {
                instance_id: saved,
                ..
            }) if saved == instance_id
        ));
        assert_eq!(
            local_auth_state(&config).unwrap().unwrap().status,
            CredentialAuthorization::Authorized
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
