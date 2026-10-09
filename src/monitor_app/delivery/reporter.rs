pub(super) async fn prepare_reporter(
    config: &mut ClientConfig,
    host: &mut xsoc::HostIdentity,
    command: ClientCommand,
    shutdown: &ShutdownSignal,
) -> anyhow::Result<Option<Reporter>> {
    loop {
        // A completed pairing is the normal service-startup state.  The
        // fallback below intentionally exposes only the previously active
        // credential while a replacement pairing is incomplete, so it returns
        // None for StoredPairingState::Active.
        if let Some(snapshot) = pairing::reporter_snapshot_for_current_active_state(config)? {
            return apply_reporter_snapshot(snapshot, config, host).map(Some);
        }
        if let Some(snapshot) = pairing::existing_reporter_for_run(config)? {
            return apply_reporter_snapshot(snapshot, config, host).map(Some);
        }
        if command != ClientCommand::Run {
            anyhow::bail!("awaiting_pairing: use pair before delivery");
        }
        info!(
            client_state = "awaiting_pairing",
            "stop the service and complete CLI pairing"
        );
        tokio::select! { _=shutdown.cancelled()=>return Ok(None), _=tokio::time::sleep(Duration::from_secs(60))=>{} }
    }
}

fn apply_reporter_snapshot(
    snapshot: pairing::ReporterSnapshot,
    config: &mut ClientConfig,
    host: &mut xsoc::HostIdentity,
) -> anyhow::Result<Reporter> {
    let reporter = snapshot.apply(config, host);
    // The status endpoint can already be published while startup is waiting
    // for authorization. Keep its identity and effective configuration aligned
    // with the same snapshot now used for collection and delivery.
    // A prior generation's ACK does not verify this reporter, even when only
    // its credential changed. Clear that evidence before publishing the new
    // binding/configuration so concurrent status readers cannot combine them.
    for field in [
        "last_ack_at",
        "last_delivery_attempt_at",
        "last_delivery_result",
        "last_http_status",
        "last_error_code",
    ] {
        xsoc::runtime_status::observe(field, serde_json::Value::Null);
    }
    xsoc::runtime_status::observe("binding_generation", serde_json::json!(host.id));
    xsoc::runtime_status::observe(
        "effective_revision",
        serde_json::json!(xcsc_cli::revision(&serde_json::to_vec(config)?)),
    );
    Ok(reporter)
}
