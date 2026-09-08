#[cfg(all(test, unix))]
use std::io::{Read, Write};
use std::{
    fs,
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
use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};
use uuid::Uuid;

#[cfg(windows)]
use host_monitor::service;

#[cfg(target_os = "linux")]
mod systemd;

#[cfg(windows)]
pub(crate) fn entry() -> anyhow::Result<()> {
    #[cfg(windows)]
    if service::windows_service_requested(std::env::args_os()) {
        return windows_service_host::dispatch();
    }

    init_tracing()?;
    build_runtime()?.block_on(run_client(platform_ready_callback()))
}

pub(crate) fn platform_ready_callback() -> Option<fn() -> anyhow::Result<bool>> {
    #[cfg(target_os = "linux")]
    {
        Some(systemd::report_ready)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

pub(crate) fn init_tracing() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "host_monitor=info".into()),
        )
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialize logging: {error}"))?;
    Ok(())
}

#[cfg(windows)]
fn build_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to initialize the async runtime")
}

#[cfg(windows)]
async fn run_client(ready: Option<fn() -> anyhow::Result<bool>>) -> anyhow::Result<()> {
    let (config, command) = ClientConfig::load_from_args().context("service configuration")?;
    execute_config(config, command, ready).await
}
pub(crate) async fn execute_config(
    mut config: ClientConfig,
    command: ClientCommand,
    ready: Option<fn() -> anyhow::Result<bool>>,
) -> anyhow::Result<()> {
    if command == ClientCommand::Status {
        return print_local_status(&config);
    }
    if command == ClientCommand::Doctor && !config.doctor_delivery {
        return run_read_only_doctor(&config).await;
    }
    let _maintenance = if matches!(
        command,
        ClientCommand::Run | ClientCommand::Once | ClientCommand::Doctor
    ) {
        Some(
            host_monitor::maintenance::Guard::acquire(&config.state_dir)
                .context("service maintenance lock")?,
        )
    } else {
        None
    };
    let session = match command {
        ClientCommand::Run | ClientCommand::Once | ClientCommand::Doctor => {
            Some(std::sync::Arc::new(
                sarmg_client_runtime::ClientSession::open(&config.state_dir)
                    .context("failed to acquire the exclusive Client delivery session")?,
            ))
        }
        ClientCommand::Pair | ClientCommand::Probe | ClientCommand::Status => None,
    };
    let shutdown = install_process_shutdown_signal().context("service shutdown handler")?;
    let mut host = if command == ClientCommand::Probe {
        transient_host_identity(Uuid::new_v4())
    } else if pairing::has_current_authorized_identity(&config)
        .context("service authorization state")?
    {
        load_host_identity(&config.state_dir).context("service host identity")?
    } else {
        transient_host_identity(Uuid::new_v4())
    };
    if shutdown.is_requested() {
        return Ok(());
    }
    if command == ClientCommand::Pair {
        anyhow::bail!("use the public CLI pairing entry");
    }

    let _status = if command == ClientCommand::Run {
        Some(
            host_monitor::runtime_status::publish(
                &config.state_dir,
                host.id.to_string(),
                crate::cli_common::revision(&serde_json::to_vec(&config)?),
            )
            .context("service status IPC")?,
        )
    } else {
        None
    };
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
    .context("service durable spool")?;
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

pub(crate) mod diagnostics;

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
mod windows_service_host;

#[cfg(test)]
mod tests;
