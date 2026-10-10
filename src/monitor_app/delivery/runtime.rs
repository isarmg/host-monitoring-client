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

fn advertise_first_sampling_cadence(
    report: &mut ClientReport,
    configured: Duration,
    first_report: &mut bool,
) {
    if *first_report {
        // The first sample measures only the time since sampler construction. The Server uses
        // this field to estimate when the next report is due; rate fields were already computed
        // from the measured elapsed time in SystemSampler::collect.
        report.interval_seconds = configured.as_secs_f64();
        *first_report = false;
    }
}

pub(super) async fn run_loop(
    config: ClientConfig,
    host: xsoc::HostIdentity,
    mut sampler: SystemSampler,
    spool: Spool,
    reporter: Reporter,
    process_shutdown: &ShutdownSignal,
) -> anyhow::Result<()> {
    let (delivery_trigger, delivery_receiver) = xcsc::runtime::DeliveryWake::channel();
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let (host_sender, host_receiver) = watch::channel(host.clone());
    let driver =
        HostDeliveryDriver::new(config.clone(), host, spool.clone(), reporter, host_sender);
    let worker = xcsc::runtime::DeliveryWorker::new(driver, config.jitter_percent)?;
    let mut delivery_worker = tokio::spawn(worker.run(delivery_receiver, shutdown_receiver));

    let mut spool_read_health = SpoolHealth::default();
    let mut spool_write_health = SpoolHealth::default();
    let mut cadence = SamplingCadence::starting_now();
    let mut first_report = true;

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
                verify_logging()?;
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
                xsoc::runtime_status::observe("last_collection_at",serde_json::json!(chrono::Utc::now().timestamp()));
                let mut report = sampler.collect(
                    host_receiver.borrow().clone(),
                    config.slow_interval_seconds,
                    pending,
                );
                advertise_first_sampling_cadence(&mut report, config.interval(), &mut first_report);
                spool_write_health.try_enqueue(&spool, &report)?;
                if !delivery_trigger.notify() {
                    warn!(report_id = %report.report_id, "delivery worker stopped before notification");
                }
                cadence.schedule_next(
                    xcsc::runtime::sampling_jitter(config.interval(), config.jitter_percent)?,
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
    host: xsoc::HostIdentity,
    spool: Spool,
    reporter: Reporter,
    host_updates: watch::Sender<xsoc::HostIdentity>,
    otlp_queue: Option<std::sync::Arc<OtlpQueue>>,
    last_waiting_notice: Option<Uuid>,
}

impl HostDeliveryDriver {
    fn new(
        config: ClientConfig,
        host: xsoc::HostIdentity,
        spool: Spool,
        reporter: Reporter,
        host_updates: watch::Sender<xsoc::HostIdentity>,
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

impl xcsc::runtime::ClientDeliveryDriver for HostDeliveryDriver {
    type Probe = HostRecoveryProbe;
    type Failure = xsoc::transport::SendError;
    type Error = anyhow::Error;

    fn recover(&self) -> xcsc::runtime::DeliveryFuture<anyhow::Result<Self::Probe>> {
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
    ) -> anyhow::Result<xcsc::runtime::RecoveryUpdate> {
        use xcsc::runtime::RecoveryUpdate;
        let (probe, snapshot) = match probe {
            HostRecoveryProbe::Credential(snapshot) => {
                (pairing::local_progress(&self.config)?, Some(*snapshot))
            }
            HostRecoveryProbe::Pairing(probe) => (probe, None),
        };
        let poll_after = match probe {
            Some(PairingProgress::Waiting(waiting)) => {
                if self.last_waiting_notice != Some(waiting.request_id) {
                    info!(client_state = "awaiting_authorization", event = "xsoc.authorization.awaiting",
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
            let reporter = apply_reporter_snapshot(snapshot, &mut self.config, &mut self.host)?;
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
            info!(event = "xsoc.authorization.active", instance_id = %self.host.id, client_state = "authorized", request_id = %revision.1,
                "current bound credential snapshot loaded; queued reports will be retried");
            return Ok(RecoveryUpdate::Renewed { poll_after });
        }
        Ok(RecoveryUpdate::Unchanged { poll_after })
    }

    fn batch(&self) -> xcsc::runtime::DeliveryFuture<anyhow::Result<FlushOutcome>> {
        let spool = self.spool.clone();
        let reporter = self.reporter.clone();
        let otlp = self.otlp_queue.clone();
        Box::pin(async move { flush_spool(&spool, &reporter, otlp.as_deref()).await })
    }

    fn classify_failure(
        &mut self,
        error: &Self::Failure,
    ) -> xcsc::runtime::DeliveryResponse {
        use xcsc::runtime::DeliveryResponse;
        if !error.is_unauthorized() {
            return DeliveryResponse::Retry;
        }
        match pairing::mark_reauth_required_if_current(
            &self.config,
            self.reporter.credential_revision(),
            format!(
                "delivery requires authorization recovery: {}",
                error.stable_code()
            ),
        ) {
            Ok(false) => {
                warn!(event = "xsoc.authorization.stale_failure", instance_id = %self.host.id, "ignored a stale authorization failure from a superseded reporter");
                DeliveryResponse::RetryAfterRecovery
            }
            Ok(true) => {
                error!(
                    event = "xsoc.authorization.required", instance_id = %self.host.id, error_code = %error.stable_code().to_ascii_lowercase(),
                    client_state = "reauth_required",
                    "the paired credential or protocol is no longer accepted. Run `xsoc pair recover --interactive`: {error}"
                );
                DeliveryResponse::AuthorizationRequired
            }
            Err(state_error) => {
                warn!(event = "xsoc.authorization.state_failed", instance_id = %self.host.id, error_code = "state_invalid", error = %state_error, "authorization state validation failed");
                DeliveryResponse::AuthorizationRequired
            }
        }
    }

    fn recovery_failed(&self, error: &anyhow::Error, retry_in: Duration) {
        warn!(
            event = "xsoc.authorization.poll_failed", instance_id = %self.host.id, error_code = "state_unavailable", error = %error,
            retry_seconds = retry_in.as_secs_f64(),
            "browser pairing state could not be checked: {error}"
        );
    }

    fn local_queue_failed(&self, error: &anyhow::Error, consecutive: u32) {
        warn!(
            event = "xsoc.queue.failed", instance_id = %self.host.id, error_code = "queue_unavailable", error = %error,
            consecutive_failures = consecutive,
            "local spool delivery failed: {error}"
        );
    }

    fn delivery_failed(&self, error: &Self::Failure, retry_in: Option<Duration>) {
        if let Some(delay) = retry_in {
            warn!(
                event = "xsoc.delivery.failed", instance_id = %self.host.id, error_code = %error.stable_code().to_ascii_lowercase(),
                pending = self.spool.pending_count().unwrap_or(0),
                retry_seconds = delay.as_secs_f64(),
                "telemetry delivery failed: {error}"
            );
        }
    }
}
