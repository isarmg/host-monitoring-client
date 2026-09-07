use std::{
    fs,
    io::{Read, Write},
    time::{Duration, Instant},
};

use anyhow::Context;
use host_monitor::{
    ClientCommand, ClientConfig, OutputMode, SystemSampler,
    collectors::{load_host_identity, transient_host_identity},
    model::ClientReport,
    pairing::{self, PairingProgress},
    service::{ShutdownSignal, shutdown_channel},
    spool::Spool,
    transport::Reporter,
};
use sarmg_client_runtime::RetryBackoff;
use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};
use uuid::Uuid;

const MAX_POST_COMMIT_EVENT_WARNING_DETAIL_BYTES: usize = 768;

#[cfg(windows)]
use host_monitor::service;

#[cfg(target_os = "linux")]
mod systemd;

pub(crate) fn entry() -> anyhow::Result<()> {
    #[cfg(windows)]
    if service::windows_service_requested(std::env::args_os()) {
        return windows_service_host::dispatch();
    }

    init_tracing()?;
    build_runtime()?.block_on(run_client(platform_ready_callback()))
}

fn platform_ready_callback() -> Option<fn() -> anyhow::Result<bool>> {
    #[cfg(target_os = "linux")]
    {
        Some(systemd::report_ready)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn init_tracing() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "host_monitor=info".into()),
        )
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialize logging: {error}"))?;
    Ok(())
}

fn build_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to initialize the async runtime")
}

async fn run_client(ready: Option<fn() -> anyhow::Result<bool>>) -> anyhow::Result<()> {
    let (mut config, command) = ClientConfig::load_from_args()?;
    if command == ClientCommand::Status {
        return print_local_status(&config);
    }
    if command == ClientCommand::Doctor && !config.doctor_delivery {
        return run_read_only_doctor(&config).await;
    }
    let session = match command {
        ClientCommand::Run | ClientCommand::Once | ClientCommand::Doctor => {
            Some(std::sync::Arc::new(
                sarmg_client_runtime::ClientSession::open(&config.state_dir)
                    .context("failed to acquire the exclusive Client delivery session")?,
            ))
        }
        ClientCommand::Pair | ClientCommand::Probe | ClientCommand::Status => None,
    };
    let shutdown = install_process_shutdown_signal()?;
    let mut host = if command == ClientCommand::Probe {
        transient_host_identity(Uuid::new_v4())
    } else if pairing::has_current_authorized_identity(&config)? {
        load_host_identity(&config.state_dir)?
    } else {
        transient_host_identity(Uuid::new_v4())
    };
    if shutdown.is_requested() {
        return Ok(());
    }
    if command == ClientCommand::Pair {
        return run_pairing(&mut config, host, &shutdown).await;
    }

    let mut sampler = SystemSampler::new();
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => return Ok(()),
        _ = tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL) => {}
    }

    if command == ClientCommand::Probe {
        let report = sampler.collect(host, config.slow_interval_seconds, 0);
        if shutdown.is_requested() {
            return Ok(());
        }
        match config.output_mode {
            OutputMode::Json => println!("{}", serde_json::to_string_pretty(&report)?),
            OutputMode::Human => println!(
                "Local collection succeeded: {} logical CPUs, {} network interfaces, {} disks, {} capabilities ({} collector errors).",
                report.system.cpu.logical_count,
                report.system.networks.len(),
                report.system.disks.len(),
                report.capabilities.len(),
                report.client.collector_errors
            ),
        }
        return Ok(());
    }

    let spool = Spool::from_session(
        session.context("delivery requires an Client session")?,
        config.spool_max_bytes,
    )
    .with_context(|| format!("failed to open spool in {}", config.state_dir.display()))?;
    // A service becomes ready only after configuration, host identity, collectors
    // and durable spool have all initialized. Network authorization is deliberately
    // not part of bootstrap: an unpaired service must remain healthy while it waits
    // for browser approval.
    if let Some(report_ready) = ready
        && !report_ready()?
    {
        return Ok(());
    }
    let Some(reporter) = prepare_reporter(&mut config, &mut host, command, &shutdown).await? else {
        info!("shutdown signal received while waiting for browser pairing");
        return Ok(());
    };

    if matches!(command, ClientCommand::Once | ClientCommand::Doctor) {
        let outcome = run_once(
            &config,
            host.clone(),
            &mut sampler,
            &spool,
            reporter,
            &shutdown,
        )
        .await?;
        if outcome == delivery::RunOnceOutcome::Shutdown {
            info!("shutdown signal received during one-shot delivery");
            return Ok(());
        }
        if command == ClientCommand::Doctor {
            let delivery = serde_json::json!({
                "schema_version": 1,
                "command": "doctor",
                "status": "healthy",
                "mode": "delivery",
                "host_id": host.id,
                "endpoint": config.endpoint,
                "spool_pending_batches": spool.pending_count()?,
                "checks": [
                    "configuration",
                    "state-directory",
                    "local-collection",
                    "host-credential",
                    "server-delivery",
                    "spool"
                ]
            });
            match config.output_mode {
                OutputMode::Json => {
                    println!("{}", serde_json::to_string_pretty(&delivery)?)
                }
                OutputMode::Human => println!(
                    "host-monitor doctor: healthy; end-to-end delivery succeeded and the spool is drained."
                ),
            }
            return Ok(());
        }
        match config.output_mode {
            OutputMode::Json => println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": 1,
                    "command": "once",
                    "status": "delivered",
                    "host_id": host.id,
                    "spool_pending_batches": spool.pending_count()?
                }))?
            ),
            OutputMode::Human => println!("One telemetry snapshot was delivered successfully."),
        }
        return Ok(());
    }

    info!(host_id = %host.id, "read-only telemetry client started");
    run_loop(config, host, sampler, spool, reporter, &shutdown).await
}

async fn run_pairing(
    config: &mut ClientConfig,
    host: host_monitor::HostIdentity,
    shutdown: &ShutdownSignal,
) -> anyhow::Result<()> {
    let tray_cancel = tray_pair_cancel_signal(config.tray_cancel_event.as_deref())?;
    let tray_deadline = config
        .tray_deadline_seconds
        .map(|seconds| Instant::now() + Duration::from_secs(seconds));
    let session = tokio::select! {
        result = pairing::start_or_resume(config, &host) => result?,
        outcome = wait_for_pairing_abort(shutdown, tray_cancel.as_ref(), tray_deadline) => {
            match outcome? {
                PairingWait::Shutdown => return Ok(()),
                outcome => stop_tray_pairing(config, outcome)?,
            }
            unreachable!()
        }
    };
    let expected_generation = session.generation;
    let expected_request_id = session.request_id;
    // The pending state carries its exact report/pairing endpoints. Keep the
    // currently active credential bound to its current endpoint until the new
    // pairing transaction reaches Active. Denial or expiry changes neither.
    if config.tray_events {
        emit_tray_pair_event(serde_json::json!({
            "event": "pairing_waiting",
            "version": env!("CARGO_PKG_VERSION"),
            "request_id": session.request_id,
            "generation": session.generation,
            "activation_url": session.activation_url,
            "pairing_endpoint": config.pairing_endpoint(),
            "expires_at": session.expires_at,
            "poll_interval": session.poll_interval
        }))?;
    } else {
        match config.output_mode {
            OutputMode::Human => println!(
                "Authorize this host in your browser:\n{}\n\nThe request expires at {}. This command may be interrupted; `run` will continue polling the saved request.",
                session.activation_url, session.expires_at
            ),
            OutputMode::Json => println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "schema_version": 1,
                    "command": "pair",
                    "status": "pairing_waiting",
                    "request_id": session.request_id,
                    "activation_url": session.activation_url,
                    "expires_at": session.expires_at,
                    "resumable": true
                }))?
            ),
        }
    }

    let expected_activation_instance = if config.tray_activation_stdin {
        let activation_code = tokio::select! {
            biased;
            outcome = wait_for_pairing_abort(shutdown, tray_cancel.as_ref(), tray_deadline) => {
                match outcome? {
                    PairingWait::Shutdown => return Ok(()),
                    outcome => stop_tray_pairing(config, outcome)?,
                }
                unreachable!()
            }
            result = spawn_tray_activation_code_reader()? => {
                result.context("authorization-key reader stopped unexpectedly")??
            }
        };
        let activated = tokio::select! {
            biased;
            outcome = wait_for_pairing_abort(shutdown, tray_cancel.as_ref(), tray_deadline) => {
                match outcome? {
                    PairingWait::Shutdown => return Ok(()),
                    outcome => stop_tray_pairing(config, outcome)?,
                }
                unreachable!()
            }
            result = pairing::activate_pending_with_code(
                config,
                expected_generation,
                expected_request_id,
                activation_code.as_str()?,
            ) => result?,
        };
        drop(activation_code);
        activated
    } else {
        None
    };

    let mut network_backoff = RetryBackoff::new(
        Duration::from_secs(1),
        Duration::from_secs(60),
        config.jitter_percent,
    )?;
    loop {
        let polled = tokio::select! {
            result = pairing::poll_existing(config) => result,
            outcome = wait_for_pairing_abort(shutdown, tray_cancel.as_ref(), tray_deadline) => {
                match outcome? {
                    PairingWait::Shutdown => return Ok(()),
                    outcome => stop_tray_pairing(config, outcome)?,
                }
                unreachable!()
            }
        };
        match polled {
            Ok(Some(PairingProgress::Creating { generation, .. })) => {
                if generation != expected_generation {
                    anyhow::bail!(
                        "this pairing request was superseded by another request; no credentials were changed"
                    );
                }
                anyhow::bail!(
                    "current pairing poll left the request in its creating phase; no credentials were changed"
                );
            }
            Ok(Some(PairingProgress::Waiting(waiting))) => {
                if waiting.generation != expected_generation
                    || waiting.request_id != expected_request_id
                {
                    anyhow::bail!(
                        "this pairing request was superseded by another request; no credentials were changed"
                    );
                }
                network_backoff.reset();
                let delay = Duration::from_secs(waiting.poll_interval);
                match wait_for_pairing_control(delay, shutdown, tray_cancel.as_ref(), tray_deadline)
                    .await?
                {
                    PairingWait::Elapsed => {}
                    PairingWait::Shutdown => {
                        if config.tray_events {
                            emit_tray_pair_event(serde_json::json!({
                                "event": "pairing_interrupted",
                                "version": env!("CARGO_PKG_VERSION"),
                                "request_id": waiting.request_id
                            }))?;
                        } else {
                            match config.output_mode {
                                OutputMode::Human => println!(
                                    "Pairing remains pending. Re-run `host-monitor pair` to continue this saved request.\n{}",
                                    waiting.activation_url
                                ),
                                OutputMode::Json => println!(
                                    "{}",
                                    serde_json::to_string(&serde_json::json!({
                                        "schema_version": 1,
                                        "command": "pair",
                                        "status": "interrupted",
                                        "request_id": waiting.request_id,
                                        "activation_url": waiting.activation_url,
                                        "resumable": true
                                    }))?
                                ),
                            }
                        }
                        return Ok(());
                    }
                    outcome => stop_tray_pairing(config, outcome)?,
                }
            }
            Ok(Some(PairingProgress::Active {
                generation,
                request_id,
                instance_id,
                report_endpoint,
            })) => {
                if generation != expected_generation
                    || request_id != expected_request_id
                    || report_endpoint != config.endpoint
                    || expected_activation_instance.is_some_and(|expected| expected != instance_id)
                {
                    anyhow::bail!(
                        "this pairing request was superseded or returned a different server endpoint; no configuration was committed"
                    );
                }
                let config_path = pairing::commit_active_configuration(
                    config,
                    generation,
                    request_id,
                    instance_id,
                    &report_endpoint,
                )?;
                let result = serde_json::json!({
                    "status": "paired",
                    "request_id": request_id,
                    "instance_id": instance_id,
                    "config": config_path,
                    "endpoint": report_endpoint,
                    "message": "browser authorization succeeded; the host credential was stored privately"
                });
                if config.tray_events {
                    let event_result = emit_tray_pair_event(serde_json::json!({
                        "event": "paired",
                        "version": env!("CARGO_PKG_VERSION"),
                        "request_id": request_id,
                        "instance_id": instance_id,
                        "endpoint": report_endpoint
                    }));
                    if let Err(error) = event_result {
                        write_post_commit_pair_event_warning(&error);
                    }
                } else {
                    match config.output_mode {
                        OutputMode::Json => {
                            println!("{}", serde_json::to_string_pretty(&result)?)
                        }
                        OutputMode::Human => println!(
                            "Pairing succeeded. Host credential stored in {} and reporting to {}.",
                            config_path.display(),
                            report_endpoint
                        ),
                    }
                }
                return Ok(());
            }
            Ok(Some(PairingProgress::Denied {
                generation,
                request_id,
                activation_url,
            })) => {
                if generation != expected_generation || request_id != expected_request_id {
                    anyhow::bail!(
                        "this pairing request was superseded by another request; no credentials were changed"
                    );
                }
                anyhow::bail!(
                    "browser pairing request {request_id} was denied; run `host-monitor pair` to \
                     create a new request ({activation_url})"
                );
            }
            Ok(Some(PairingProgress::Expired {
                generation,
                request_id,
                activation_url,
            })) => {
                if generation != expected_generation || request_id != expected_request_id {
                    anyhow::bail!(
                        "this pairing request was superseded by another request; no credentials were changed"
                    );
                }
                anyhow::bail!(
                    "browser pairing request {request_id} expired; run `host-monitor pair` to \
                     create a new request ({activation_url})"
                );
            }
            Ok(None) => anyhow::bail!("the persisted pairing request disappeared"),
            Err(error) => {
                if chrono::Utc::now() >= session.expires_at {
                    if !config.tray_events && config.output_mode == OutputMode::Json {
                        println!(
                            "{}",
                            serde_json::to_string(&serde_json::json!({
                                "schema_version": 1,
                                "command": "pair",
                                "status": "expired",
                                "request_id": session.request_id,
                                "resumable": false,
                                "error": {
                                    "code": "pairing_expired",
                                    "message": concat!(
                                        "the saved pairing request expired while the server was unreachable; ",
                                        "use pair --replace-pending-pairing to explicitly create a fresh request"
                                    )
                                }
                            }))?
                        );
                    }
                    anyhow::bail!(
                        "browser pairing request {} expired while the server was unreachable; run \
                         `host-monitor pair --replace-pending-pairing` to explicitly create a fresh request",
                        session.request_id
                    );
                }
                let delay = network_backoff.next_delay()?;
                warn!(
                    retry_seconds = delay.as_secs_f64(),
                    "pairing status could not be checked; the saved request will be retried: {error}"
                );
                match wait_for_pairing_control(delay, shutdown, tray_cancel.as_ref(), tray_deadline)
                    .await?
                {
                    PairingWait::Elapsed => {}
                    PairingWait::Shutdown => return Ok(()),
                    outcome => stop_tray_pairing(config, outcome)?,
                }
            }
        }
    }
}

/// A bounded, in-memory authorization key received only from the elevated
/// tray broker's anonymous stdin. The backing allocation is overwritten when
/// it leaves scope; the value is never formatted or included in an error.
struct TrayActivationCode(Vec<u8>);

impl TrayActivationCode {
    fn as_str(&self) -> anyhow::Result<&str> {
        std::str::from_utf8(&self.0).context("authorization key is not valid UTF-8")
    }
}

impl Drop for TrayActivationCode {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

fn read_tray_activation_code() -> anyhow::Result<TrayActivationCode> {
    let mut bytes = Vec::with_capacity(258);
    std::io::stdin()
        .lock()
        .take(258)
        .read_to_end(&mut bytes)
        .context("failed to receive the authorization key from the tray broker")?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > 256 {
        bytes.fill(0);
        anyhow::bail!("authorization key exceeds the 256-byte limit");
    }
    let code = TrayActivationCode(bytes);
    crate_validate_tray_activation_code(code.as_str()?)?;
    Ok(code)
}

type TrayActivationCodeReceiver =
    tokio::sync::oneshot::Receiver<anyhow::Result<TrayActivationCode>>;

fn spawn_tray_activation_code_reader() -> anyhow::Result<TrayActivationCodeReceiver> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("host-monitoring-pair-stdin".into())
        .spawn(move || {
            // A standard-input read has no portable asynchronous cancellation. Keep it off the
            // Tokio blocking pool so process shutdown does not wait for a broker that failed to
            // close its pipe. If the receiver was cancelled, SendError drops and zeroizes the
            // returned activation code on this thread.
            let _ = sender.send(read_tray_activation_code());
        })
        .context("failed to start the authorization-key reader")?;
    Ok(receiver)
}

fn crate_validate_tray_activation_code(value: &str) -> anyhow::Result<()> {
    host_monitor::tray_support::validate_activation_code(value).map(|_| ())
}

async fn wait_for_pairing_abort(
    shutdown: &ShutdownSignal,
    tray_cancel: Option<&ShutdownSignal>,
    tray_deadline: Option<Instant>,
) -> anyhow::Result<PairingWait> {
    tokio::select! {
        _ = shutdown.cancelled() => Ok(PairingWait::Shutdown),
        _ = optional_pair_cancel(tray_cancel) => Ok(PairingWait::Cancelled),
        _ = optional_pair_deadline(tray_deadline) => Ok(PairingWait::Deadline),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PairingWait {
    Elapsed,
    Shutdown,
    Cancelled,
    Deadline,
}

async fn wait_for_pairing_control(
    delay: Duration,
    shutdown: &ShutdownSignal,
    tray_cancel: Option<&ShutdownSignal>,
    tray_deadline: Option<Instant>,
) -> anyhow::Result<PairingWait> {
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(PairingWait::Elapsed),
        _ = shutdown.cancelled() => Ok(PairingWait::Shutdown),
        _ = optional_pair_cancel(tray_cancel) => Ok(PairingWait::Cancelled),
        _ = optional_pair_deadline(tray_deadline) => Ok(PairingWait::Deadline),
    }
}

async fn optional_pair_cancel(signal: Option<&ShutdownSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
    }
}

async fn optional_pair_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

fn stop_tray_pairing(config: &ClientConfig, outcome: PairingWait) -> anyhow::Result<()> {
    let (event, message) = match outcome {
        PairingWait::Cancelled => ("pairing_cancelled", "tray pairing was cancelled safely"),
        PairingWait::Deadline => (
            "pairing_timeout",
            "tray pairing reached its configured safety deadline",
        ),
        _ => return Ok(()),
    };
    if config.tray_events {
        emit_tray_pair_event(serde_json::json!({
            "event": event,
            "version": env!("CARGO_PKG_VERSION")
        }))?;
    }
    anyhow::bail!(message)
}

fn tray_pair_cancel_signal(name: Option<&str>) -> anyhow::Result<Option<ShutdownSignal>> {
    let Some(name) = name else {
        return Ok(None);
    };
    #[cfg(not(windows))]
    {
        let _ = name;
        anyhow::bail!("tray cancellation events are available only on Windows");
    }
    #[cfg(windows)]
    {
        windows_pair_cancel::open(name).map(Some)
    }
}

fn emit_tray_pair_event(event: serde_json::Value) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &event)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn write_post_commit_pair_event_warning(error: &anyhow::Error) {
    let mut detail = format!("{error:#}");
    if detail.len() > MAX_POST_COMMIT_EVENT_WARNING_DETAIL_BYTES {
        let ellipsis = "…";
        let mut boundary = MAX_POST_COMMIT_EVENT_WARNING_DETAIL_BYTES - ellipsis.len();
        while !detail.is_char_boundary(boundary) {
            boundary -= 1;
        }
        detail.truncate(boundary);
        detail.push_str(ellipsis);
    }
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(
        stderr,
        "Warning: pairing credentials and configuration were committed, but the paired event could not be written: {detail}"
    );
    let _ = stderr.flush();
}

mod diagnostics;

use diagnostics::{print_local_status, run_read_only_doctor};

mod delivery;

use delivery::{prepare_reporter, run_loop, run_once};

fn install_process_shutdown_signal() -> anyhow::Result<ShutdownSignal> {
    #[cfg(windows)]
    if let Some(signal) = windows_service_host::shutdown_signal() {
        return Ok(signal);
    }

    let (controller, signal) = shutdown_channel();

    #[cfg(unix)]
    {
        // Tokio permanently replaces the operating system's default handling after the first
        // signal stream is registered. Keep both streams alive and continuously polled for the
        // process lifetime; recreating them around individual waits leaves windows in which a
        // SIGINT/SIGTERM is consumed globally but observed by no receiver.
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::spawn(async move {
            loop {
                let (name, received) = tokio::select! {
                    signal = interrupt.recv() => ("SIGINT", signal),
                    signal = terminate.recv() => ("SIGTERM", signal),
                };
                if received.is_none() {
                    error!(signal = name, "process signal listener closed unexpectedly");
                    controller.request_shutdown();
                    return;
                }
                controller.request_shutdown();
            }
        });
    }

    #[cfg(not(unix))]
    tokio::spawn(async move {
        if let Err(error) = tokio::signal::ctrl_c().await {
            error!("shutdown handler failed: {error}");
        }
        controller.request_shutdown();
    });

    Ok(signal)
}

#[cfg(windows)]
mod windows_pair_cancel;

#[cfg(windows)]
mod windows_service_host;

#[cfg(test)]
mod tests;
