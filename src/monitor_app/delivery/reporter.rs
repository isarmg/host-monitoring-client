pub(super) async fn prepare_reporter(
    config: &mut ClientConfig, host: &mut host_monitor::HostIdentity,
    command: ClientCommand, shutdown: &ShutdownSignal,
) -> anyhow::Result<Option<Reporter>> {
    loop {
        if let Some(snapshot) = pairing::existing_reporter_for_run(config)? {
            return Ok(Some(snapshot.apply(config, host)));
        }
        if command != ClientCommand::Run { anyhow::bail!("awaiting_pairing: use pair before delivery"); }
        info!(client_state="awaiting_pairing", "stop the service and complete CLI pairing");
        tokio::select! { _=shutdown.cancelled()=>return Ok(None), _=tokio::time::sleep(Duration::from_secs(60))=>{} }
    }
}
