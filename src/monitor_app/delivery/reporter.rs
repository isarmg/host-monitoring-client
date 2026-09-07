pub(super) async fn prepare_reporter(
    config: &mut ClientConfig,
    host: &mut host_monitor::HostIdentity,
    command: ClientCommand,
    shutdown: &ShutdownSignal,
) -> anyhow::Result<Option<Reporter>> {
    let mut backoff = RetryBackoff::new(
        Duration::from_secs(1),
        Duration::from_secs(300),
        config.jitter_percent,
    )?;
    let mut idle_backoff = RetryBackoff::new(
        Duration::from_secs(1),
        Duration::from_secs(60),
        config.jitter_percent,
    )?;
    let mut last_authorization_notice: Option<(&'static str, Option<Uuid>)> = None;
    loop {
        if command == ClientCommand::Run
            && let Some(snapshot) = pairing::existing_reporter_for_run(config)?
        {
            return Ok(Some(snapshot.apply(config, host)));
        }
        let progress = tokio::select! {
            result = pairing::poll_existing(config) => result,
            _ = shutdown.cancelled() => return Ok(None),
        };
        match progress {
            Ok(Some(PairingProgress::Creating { .. })) => {
                continue;
            }
            Ok(Some(PairingProgress::Waiting(waiting))) => {
                if command != ClientCommand::Run {
                    anyhow::bail!(
                        "browser authorization is still pending; open {}",
                        waiting.activation_url
                    );
                }
                backoff.reset();
                idle_backoff.reset();
                let notice = ("pending", Some(waiting.request_id));
                if last_authorization_notice != Some(notice) {
                    info!(
                        client_state = "awaiting_authorization",
                        request_id = %waiting.request_id,
                        activation_url = %waiting.activation_url,
                        "browser authorization is pending"
                    );
                    last_authorization_notice = Some(notice);
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(waiting.poll_interval)) => {},
                    _ = shutdown.cancelled() => return Ok(None),
                }
                continue;
            }
            Ok(Some(PairingProgress::Active {
                generation,
                request_id,
                instance_id,
                report_endpoint,
            })) => {
                let reporter = pairing::activate_reporter_snapshot(
                    config,
                    host,
                    generation,
                    request_id,
                    instance_id,
                    &report_endpoint,
                )
                .context(
                    "paired host credential could not be loaded; run `host-monitor pair` again",
                )?;
                return Ok(Some(reporter));
            }
            Ok(Some(PairingProgress::Denied {
                generation: _,
                request_id,
                activation_url,
            })) => {
                if command != ClientCommand::Run {
                    anyhow::bail!(
                        "browser authorization request {request_id} was denied; run pair again \
                         ({activation_url})"
                    );
                }
                let notice = ("denied", Some(request_id));
                if last_authorization_notice != Some(notice) {
                    info!(
                        client_state = "awaiting_authorization",
                        %request_id,
                        "browser authorization was denied; run `host-monitor pair --server <url>`"
                    );
                    last_authorization_notice = Some(notice);
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {},
                    _ = shutdown.cancelled() => return Ok(None),
                }
                continue;
            }
            Ok(Some(PairingProgress::Expired {
                generation: _,
                request_id,
                activation_url,
            })) => {
                if command != ClientCommand::Run {
                    anyhow::bail!(
                        "browser authorization request {request_id} expired; run pair again \
                         ({activation_url})"
                    );
                }
                let notice = ("expired", Some(request_id));
                if last_authorization_notice != Some(notice) {
                    info!(
                        client_state = "awaiting_authorization",
                        %request_id,
                        "browser authorization expired; run `host-monitor pair --server <url>`"
                    );
                    last_authorization_notice = Some(notice);
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {},
                    _ = shutdown.cancelled() => return Ok(None),
                }
                continue;
            }
            Ok(None) => {}
            Err(error) if command != ClientCommand::Run => {
                return Err(error.context("failed to resume browser pairing"));
            }
            Err(error) => {
                let delay = backoff.next_delay()?;
                warn!(
                    retry_seconds = delay.as_secs_f64(),
                    "browser pairing state could not be checked; retrying: {error}"
                );
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {},
                    _ = shutdown.cancelled() => return Ok(None),
                }
                continue;
            }
        }

        if command != ClientCommand::Run {
            anyhow::bail!(
                "this host is not authorized; run `host-monitor pair --server <url>` first"
            );
        }
        let delay = idle_backoff.next_delay()?;
        let notice = ("unconfigured", None);
        if last_authorization_notice != Some(notice) {
            info!(
                client_state = "awaiting_authorization",
                retry_seconds = delay.as_secs_f64(),
                "no host credential or pending pairing request; run `host-monitor pair --server <url>`"
            );
            last_authorization_notice = Some(notice);
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {},
            _ = shutdown.cancelled() => return Ok(None),
        }
    }
}
