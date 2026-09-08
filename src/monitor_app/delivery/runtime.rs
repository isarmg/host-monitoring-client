/// A cadence is anchored to its previous deadline, not to the end of sampling.
/// If collection itself overruns a full interval, missed ticks are skipped
/// instead of emitted in a burst.
struct SamplingCadence {
    deadline: tokio::time::Instant,
}

impl SamplingCadence {
    fn starting_now() -> Self {
        Self {
            deadline: tokio::time::Instant::now(),
        }
    }

    fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }

    fn schedule_next(&mut self, interval: Duration, now: tokio::time::Instant) {
        let anchored = self.deadline + interval;
        self.deadline = if anchored > now {
            anchored
        } else {
            now + interval
        };
    }
}

pub(super) async fn run_loop(
    config: ClientConfig,
    host: host_monitor::HostIdentity,
    mut sampler: SystemSampler,
    spool: Spool,
    reporter: Reporter,
    process_shutdown: &ShutdownSignal,
) -> anyhow::Result<()> {
    let (delivery_trigger, delivery_receiver) = sarmg_client_runtime::DeliveryWake::channel();
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let (host_sender, host_receiver) = watch::channel(host.clone());
    let driver =
        HostDeliveryDriver::new(config.clone(), host, spool.clone(), reporter, host_sender);
    let worker = sarmg_client_runtime::DeliveryWorker::new(driver, config.jitter_percent)?;
    let mut delivery_worker = tokio::spawn(worker.run(delivery_receiver, shutdown_receiver));

    let mut spool_read_health = SpoolHealth::default();
    let mut spool_write_health = SpoolHealth::default();
    let mut cadence = SamplingCadence::starting_now();

    loop {
        tokio::select! {
            biased;
            result = &mut delivery_worker => {
                return delivery_worker_result(result);
            }
            _ = process_shutdown.cancelled() => {
                info!("shutdown signal received");
                let _ = shutdown_sender.send(true);
                drop(delivery_trigger);
                return stop_delivery_worker(delivery_worker).await;
            }
            _ = tokio::time::sleep_until(cadence.deadline()) => {
                // Only bounded local disk work is allowed on the cadence path.
                // Every network operation, retry and 32-report backlog batch is
                // owned by `delivery_loop` and cannot shift the next deadline.
                let pending = match spool.pending_count() {
                    Ok(count) => {
                        spool_read_health.record_success();
                        count
                    }
                    Err(error) => {
                        spool_read_health.record_failure("读取 spool 队列长度", &error)?;
                        0
                    }
                };
                host_monitor::runtime_status::observe("last_collection_at",serde_json::json!(chrono::Utc::now().timestamp()));
                let report = sampler.collect(
                    host_receiver.borrow().clone(),
                    config.slow_interval_seconds,
                    pending,
                );
                spool_write_health.try_enqueue(&spool, &report)?;
                if !delivery_trigger.notify() {
                    warn!(report_id = %report.report_id, "delivery worker stopped before notification");
                }
                cadence.schedule_next(
                    sarmg_client_runtime::sampling_jitter(config.interval(), config.jitter_percent)?,
                    tokio::time::Instant::now(),
                );
            }
        }
    }
}

fn delivery_worker_result(
    result: Result<anyhow::Result<()>, tokio::task::JoinError>,
) -> anyhow::Result<()> {
    result.map_err(|error| anyhow::anyhow!("delivery worker task failed: {error}"))?
}

async fn stop_delivery_worker(
    mut worker: tokio::task::JoinHandle<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    match tokio::time::timeout(Duration::from_secs(5), &mut worker).await {
        Ok(result) => delivery_worker_result(result),
        Err(_) => {
            warn!(
                "delivery worker did not stop within 5 seconds; cancelling it with reports preserved in spool"
            );
            worker.abort();
            let _ = worker.await;
            Ok(())
        }
    }
}

struct HostDeliveryDriver {
    config: ClientConfig,
    host: host_monitor::HostIdentity,
    spool: Spool,
    reporter: Reporter,
    host_updates: watch::Sender<host_monitor::HostIdentity>,
    otlp_queue: Option<std::sync::Arc<OtlpQueue>>,
    last_waiting_notice: Option<Uuid>,
}

impl HostDeliveryDriver {
    fn new(
        config: ClientConfig,
        host: host_monitor::HostIdentity,
        spool: Spool,
        reporter: Reporter,
        host_updates: watch::Sender<host_monitor::HostIdentity>,
    ) -> Self {
        let otlp_queue = config
            .otlp_endpoint
            .as_ref()
            .map(|_| OtlpQueue::spawn(reporter.clone()));
        Self {
            config,
            host,
            spool,
            reporter,
            host_updates,
            otlp_queue,
            last_waiting_notice: None,
        }
    }
}

impl Drop for HostDeliveryDriver {
    fn drop(&mut self) {
        if let Some(queue) = self.otlp_queue.take() {
            queue.abort();
        }
    }
}

enum HostRecoveryProbe {
    Credential(Box<pairing::ReporterSnapshot>),
    Pairing(Option<PairingProgress>),
}

impl sarmg_client_runtime::ClientDeliveryDriver for HostDeliveryDriver {
    type Probe = HostRecoveryProbe;
    type Failure = host_monitor::transport::SendError;
    type Error = anyhow::Error;

    fn recover(&self) -> sarmg_client_runtime::DeliveryFuture<anyhow::Result<Self::Probe>> {
        let config = self.config.clone();
        let revision = self.reporter.credential_revision();
        Box::pin(async move {
            // A usable local rotation must not wait for a subsequent pairing's
            // network endpoint, which may be slow or unavailable.
            if let Some(snapshot) = pairing::refresh_reporter_snapshot(&config, revision)? {
                return Ok(HostRecoveryProbe::Credential(Box::new(snapshot)));
            }
            pairing::poll_existing(&config)
                .await
                .map(HostRecoveryProbe::Pairing)
        })
    }

    fn apply_recovery(
        &mut self,
        probe: Self::Probe,
    ) -> anyhow::Result<sarmg_client_runtime::RecoveryUpdate> {
        use sarmg_client_runtime::RecoveryUpdate;
        let (probe, snapshot) = match probe {
            HostRecoveryProbe::Credential(snapshot) => {
                (pairing::local_progress(&self.config)?, Some(*snapshot))
            }
            HostRecoveryProbe::Pairing(probe) => (probe, None),
        };
        let poll_after = match probe {
            Some(PairingProgress::Waiting(waiting)) => {
                if self.last_waiting_notice != Some(waiting.request_id) {
                    info!(client_state = "awaiting_authorization", activation_url = %waiting.activation_url,
                        "open the activation URL to authorize telemetry delivery");
                    self.last_waiting_notice = Some(waiting.request_id);
                }
                Duration::from_secs(waiting.poll_interval)
            }
            Some(PairingProgress::Creating { .. }) => Duration::from_secs(2),
            Some(
                PairingProgress::Active { .. }
                | PairingProgress::Denied { .. }
                | PairingProgress::Expired { .. },
            )
            | None => Duration::from_secs(60),
        };
        let snapshot = match snapshot {
            Some(snapshot) => Some(snapshot),
            None => pairing::refresh_reporter_snapshot(
                &self.config,
                self.reporter.credential_revision(),
            )?,
        };
        if let Some(snapshot) = snapshot {
            let revision = snapshot.credential_revision();
            let reporter = snapshot.apply(&mut self.config, &mut self.host);
            let next_otlp = self
                .config
                .otlp_endpoint
                .as_ref()
                .map(|_| OtlpQueue::spawn(reporter.clone()));
            if let Some(previous) = self.otlp_queue.take() {
                previous.abort();
            }
            self.otlp_queue = next_otlp;
            self.reporter = reporter;
            let _ = self.host_updates.send(self.host.clone());
            info!(client_state = "authorized", request_id = %revision.1,
                "current bound credential snapshot loaded; queued reports will be retried");
            return Ok(RecoveryUpdate::Renewed { poll_after });
        }
        Ok(RecoveryUpdate::Unchanged { poll_after })
    }

    fn batch(&self) -> sarmg_client_runtime::DeliveryFuture<anyhow::Result<FlushOutcome>> {
        let spool = self.spool.clone();
        let reporter = self.reporter.clone();
        let otlp = self.otlp_queue.clone();
        Box::pin(async move { flush_spool(&spool, &reporter, otlp.as_deref()).await })
    }

    fn classify_failure(&mut self, error: &Self::Failure) -> sarmg_client_runtime::DeliveryResponse {
        use sarmg_client_runtime::DeliveryResponse;
        if !error.is_unauthorized() {
            return DeliveryResponse::Retry;
        }
        match pairing::mark_reauth_required_if_current(
            &self.config,
            self.reporter.credential_revision(),
            format!("the host credential was rejected with HTTP 401: {error}"),
        ) {
            Ok(false) => {
                warn!("ignored a stale 401 from a reporter superseded by newer pairing state");
                DeliveryResponse::RetryAfterRecovery
            }
            Ok(true) => {
                error!(
                    client_state = "reauth_required",
                    "the paired credential is no longer accepted. Run `host-monitor pair --server <url>`: {error}"
                );
                DeliveryResponse::AuthorizationRequired
            }
            Err(state_error) => {
                warn!("failed to validate reauth_required state: {state_error}");
                DeliveryResponse::AuthorizationRequired
            }
        }
    }

    fn recovery_failed(&self, error: &anyhow::Error, retry_in: Duration) {
        warn!(
            retry_seconds = retry_in.as_secs_f64(),
            "browser pairing state could not be checked: {error}"
        );
    }

    fn local_queue_failed(&self, error: &anyhow::Error, consecutive: u32) {
        warn!(
            consecutive_failures = consecutive,
            "local spool delivery failed: {error}"
        );
    }

    fn delivery_failed(&self, error: &Self::Failure, retry_in: Option<Duration>) {
        if let Some(delay) = retry_in {
            warn!(
                pending = self.spool.pending_count().unwrap_or(0),
                retry_seconds = delay.as_secs_f64(),
                "telemetry delivery failed: {error}"
            );
        }
    }
}
