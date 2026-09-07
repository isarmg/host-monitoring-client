/// Submit the one-time authorization key for exactly the pending generation
/// emitted to the trusted Windows tray broker.
///
/// The key stays in memory and is sent only by the Client's TLS-configured
/// client. Redirects are disabled so an HTTP 307/308 can never replay the JSON
/// body to another endpoint. A 409 is deliberately treated as ambiguous: the
/// server may have committed a previous attempt whose response was lost, so
/// the caller must keep polling this same durable request rather than create a
/// replacement and risk losing the successfully activated generation.
pub async fn activate_pending_with_code(
    config: &ClientConfig,
    generation: Uuid,
    request_id: Uuid,
    activation_code: &str,
) -> anyhow::Result<Option<Uuid>> {
    crate::tray_support::validate_activation_code(activation_code)?;
    let (activation_url, pairing_endpoint, report_endpoint, polling_secret) = {
        let transaction = lock_state(config)?;
        let store = &transaction;
        let state = load_state(store)?
            .context("the pending pairing request disappeared before activation")?;
        match state {
            StoredPairingState::Pending {
                version,
                generation: current_generation,
                request_id: current_request_id,
                activation_url,
                pairing_endpoint,
                report_endpoint,
                polling_secret,
                ..
            } => {
                validate_state_version(version)?;
                if current_generation != generation || current_request_id != request_id {
                    return Err(PairingSuperseded.into());
                }
                (
                    activation_url,
                    pairing_endpoint,
                    report_endpoint,
                    polling_secret,
                )
            }
            StoredPairingState::Activating {
                generation: current_generation,
                request_id: current_request_id,
                instance_id,
                ..
            }
            | StoredPairingState::Active {
                generation: current_generation,
                request_id: current_request_id,
                instance_id,
                ..
            } if current_generation == generation && current_request_id == request_id => {
                return Ok(Some(instance_id));
            }
            _ => {
                bail!("the pairing request is no longer pending; authorization key was not sent")
            }
        }
    };
    config
        .validate_durable_report_endpoint(&report_endpoint)
        .context("stored report endpoint is unsafe")?;
    validate_activation_url_request(&activation_url, &pairing_endpoint, request_id)?;
    let endpoint = activation_endpoint(&pairing_endpoint)?;
    let endpoint_display = endpoint.as_str().to_string();
    let client = build_client(config)?;
    let request_id_text = request_id.to_string();
    let mut headers = json_headers();
    headers.insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    let response = client
        .post_client(endpoint.as_str(), headers, serde_json::to_vec(&ActivatePairingRequest {
            request_id: &request_id_text,
            activation_code,
        })?)
        .await
        .context("failed to submit the one-time authorization key")?;
    let status = response.status;
    let content_type = pairing_response_content_type(&response);
    let body = response.body;
    let activated_instance = if status == StatusCode::CONFLICT {
        None
    } else {
        ensure_pairing_status(
            status,
            &[StatusCode::OK],
            "submit the one-time authorization key",
        )?;
        let activated: ActivatePairingResponse = parse_pairing_json(
            &body,
            &content_type,
            &endpoint_display,
            "pairing activation response",
        )?;
        debug_assert_eq!(activated.status, ActivatePairingStatus::Active);
        Some(
            Uuid::parse_str(&activated.instance_id)
                .expect("protocol rejected a non-canonical activated instance UUID"),
        )
    };

    // Network I/O never holds the credential lock. Reacquire and compare the
    // full pending transaction before allowing the caller to trust the result.
    let transaction = lock_state(config)?;
    let store = &transaction;
    let current = load_state(store)?.context("the pairing state disappeared after activation")?;
    match current {
        StoredPairingState::Pending { .. } => {
            ensure_pending_is_current(
                store,
                generation,
                request_id,
                &pairing_endpoint,
                &report_endpoint,
                &polling_secret,
            )?;
            Ok(activated_instance)
        }
        StoredPairingState::Activating {
            generation: current_generation,
            request_id: current_request_id,
            instance_id,
            ..
        }
        | StoredPairingState::Active {
            generation: current_generation,
            request_id: current_request_id,
            instance_id,
            ..
        } if current_generation == generation && current_request_id == request_id => {
            if activated_instance.is_some_and(|activated| activated != instance_id) {
                bail!(
                    "activation instance does not match the concurrently committed pairing state"
                );
            }
            Ok(Some(instance_id))
        }
        _ => Err(PairingSuperseded.into()),
    }
}
