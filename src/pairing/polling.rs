/// Inspect or advance the locally persisted pairing state once. Callers own
/// scheduling so an interactive `pair` command and an already-running service
/// can safely poll the same request without holding a lock or a socket.
pub async fn poll_existing(config: &ClientConfig) -> anyhow::Result<Option<PairingProgress>> {
    let Some(state) = load_state_for_network(config)? else {
        return Ok(None);
    };
    let state = match state {
        creating @ StoredPairingState::Creating { .. } => {
            let waiting = finish_create_request(config, creating).await?;
            return Ok(Some(PairingProgress::Waiting(waiting)));
        }
        activating @ StoredPairingState::Activating { .. } => {
            return recover_activating(config, activating).map(Some);
        }
        state => state,
    };
    let pending_for_activation = state.clone();
    let StoredPairingState::Pending {
        version,
        generation,
        request_id,
        activation_url,
        expires_at,
        poll_interval,
        pairing_endpoint,
        report_endpoint,
        bearer_secret: _,
        polling_secret,
    } = state
    else {
        return Ok(Some(progress_from_terminal(state)));
    };
    validate_state_version(version)?;
    config
        .validate_durable_report_endpoint(&report_endpoint)
        .context("stored report endpoint is unsafe")?;

    let endpoint = pairing_status_endpoint(&pairing_endpoint, request_id)?;
    let client = build_client(config)?;
    let mut headers = json_headers();
    headers.insert(
            header::AUTHORIZATION,
            pairing_authorization(&polling_secret)?,
    );
    let response = client
        .post_client(endpoint.as_str(), headers, Vec::new())
        .await
        .context("failed to poll browser pairing status")?;
    let status = response.status;
    let content_type = pairing_response_content_type(&response);
    let body = response.body;
    ensure_pairing_status(status, &[StatusCode::OK], "poll pairing status")?;
    let polled: PairingStatusResponse = parse_pairing_json(
        &body,
        &content_type,
        endpoint.as_str(),
        "pairing status response",
    )?;

    match polled.status {
        PairingStatus::Waiting => {
            if polled.instance_id.is_some() {
                bail!("waiting pairing response unexpectedly included instance_id");
            }
            Ok(Some(PairingProgress::Waiting(PairingSession {
                generation,
                request_id,
                activation_url,
                expires_at,
                poll_interval,
            })))
        }
        PairingStatus::Active => {
            let instance_id = polled
                .instance_id
                .context("active pairing response omitted instance_id")?;
            let instance_id = Uuid::parse_str(&instance_id)
                .expect("protocol rejected a non-canonical paired instance UUID");
            persist_active_credentials(config, pending_for_activation, instance_id).map(Some)
        }
        PairingStatus::Denied => {
            if polled.instance_id.is_some() {
                bail!("denied pairing response unexpectedly included instance_id");
            }
            let expected_report_endpoint = report_endpoint.clone();
            let denied = StoredPairingState::Denied {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: activation_url.clone(),
                report_endpoint,
                completed_at: Utc::now(),
            };
            compare_and_persist_pending(
                config,
                generation,
                request_id,
                &pairing_endpoint,
                &expected_report_endpoint,
                &polling_secret,
                &denied,
            )?;
            Ok(Some(PairingProgress::Denied {
                generation,
                request_id,
                activation_url,
            }))
        }
        PairingStatus::Expired => {
            if polled.instance_id.is_some() {
                bail!("expired pairing response unexpectedly included instance_id");
            }
            let expected_report_endpoint = report_endpoint.clone();
            let expired = StoredPairingState::Expired {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: activation_url.clone(),
                report_endpoint,
                completed_at: Utc::now(),
            };
            compare_and_persist_pending(
                config,
                generation,
                request_id,
                &pairing_endpoint,
                &expected_report_endpoint,
                &polling_secret,
                &expired,
            )?;
            Ok(Some(PairingProgress::Expired {
                generation,
                request_id,
                activation_url,
            }))
        }
    }
}

fn pairing_status_endpoint(
    pairing_endpoint: &str,
    request_id: Uuid,
) -> anyhow::Result<sarmg_client_secure_http::Url> {
    crate::config::validate_pairing_endpoint(pairing_endpoint)
        .context("stored pairing endpoint is unsafe")?;
    let mut endpoint = sarmg_client_secure_http::Url::parse(pairing_endpoint)
        .context("stored pairing endpoint is not a valid URL")?;
    if endpoint.query().is_some() || endpoint.fragment().is_some() {
        bail!("stored pairing endpoint must not contain a query or fragment");
    }
    let request_id = request_id.to_string();
    let mut segments = endpoint
        .path_segments_mut()
        .map_err(|_| anyhow::anyhow!("stored pairing endpoint cannot accept path segments"))?;
    segments.pop_if_empty();
    segments.push(&request_id);
    segments.push("status");
    drop(segments);
    Ok(endpoint)
}

fn load_state_for_network(config: &ClientConfig) -> anyhow::Result<Option<StoredPairingState>> {
    let transaction = lock_state(config)?;
    let store = &transaction;
    load_state(store)
}
