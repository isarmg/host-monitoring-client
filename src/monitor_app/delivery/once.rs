/// 一次性投递是正常完成，还是在保证当前报文可重试后响应关停。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunOnceOutcome {
    Delivered,
    Shutdown,
}

/// 采样一次，并保证之前由 `once`/`run` 留下的积压得到补传。
pub(super) async fn run_once(
    config: &ClientConfig,
    host: host_monitor::HostIdentity,
    sampler: &mut SystemSampler,
    spool: &Spool,
    reporter: Reporter,
    shutdown: &ShutdownSignal,
) -> anyhow::Result<RunOnceOutcome> {
    let pending = spool.pending_count()?;
    let report = sampler.collect(host.clone(), config.slow_interval_seconds, pending);
    // `flush_spool` 单轮最多发 32 份；once 是显式的一次性投递命令，因此循环到队列
    // 清空。若网络仍不可用，当前采样也入队后退出，下一次 once 可以继续恢复。
    while spool.pending_count()? > 0 {
        let Some(flush) =
            finish_before_shutdown(shutdown, flush_spool(spool, &reporter, None)).await
        else {
            return retain_once_report(spool, &report);
        };
        match flush {
            Ok(FlushOutcome::Drained | FlushOutcome::BatchComplete) => {}
            Ok(FlushOutcome::Failed(error)) => {
                spool.enqueue(&report)?;
                return Err(anyhow::anyhow!(error)
                    .context("current report was retained while stored reports remain pending"));
            }
            Err(error) => {
                spool.enqueue(&report)?;
                return Err(error.context(
                    "current report was retained because the local spool could not be flushed",
                ));
            }
        }
    }

    let Some(send) = finish_before_shutdown(shutdown, reporter.send_host_monitoring(&report)).await
    else {
        return retain_once_report(spool, &report);
    };
    if let Err(error) = send {
        if error.is_permanent() {
            return Err(anyhow::anyhow!(error)
                .context("report was rejected permanently and was not spooled"));
        }
        spool.enqueue(&report)?;
        return Err(anyhow::anyhow!(error).context("report was retained in the local spool"));
    }
    let Some(otlp) = finish_before_shutdown(shutdown, reporter.send_otlp(&report)).await else {
        // Host Monitoring has acknowledged this report already. OTLP is optional, so there is nothing
        // left to retain when shutdown wins this final best-effort export.
        return Ok(RunOnceOutcome::Shutdown);
    };
    if let Err(error) = otlp {
        warn!("optional OTLP export failed: {error}");
    }
    Ok(RunOnceOutcome::Delivered)
}

async fn finish_before_shutdown<T>(
    shutdown: &ShutdownSignal,
    operation: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => None,
        result = operation => Some(result),
    }
}

fn retain_once_report(spool: &Spool, report: &ClientReport) -> anyhow::Result<RunOnceOutcome> {
    // The request may already have reached the server when cancellation wins. Retaining the
    // same report_id makes the next delivery an idempotent retry instead of losing the sample.
    spool.enqueue(report)?;
    Ok(RunOnceOutcome::Shutdown)
}

struct OtlpQueue {
    sender: mpsc::Sender<ClientReport>,
    worker: tokio::task::JoinHandle<()>,
}

impl OtlpQueue {
    fn spawn(reporter: Reporter) -> std::sync::Arc<Self> {
        // OTLP is an optional secondary output. A bounded worker prevents a slow
        // collector from delaying host sampling or primary Host Monitoring delivery.
        let (sender, mut receiver) = mpsc::channel::<ClientReport>(128);
        let worker = tokio::spawn(async move {
            while let Some(report) = receiver.recv().await {
                if let Err(error) = reporter.send_otlp(&report).await {
                    warn!(report_id = %report.report_id, "optional OTLP export failed: {error}");
                }
            }
        });
        std::sync::Arc::new(Self { sender, worker })
    }

    fn try_export(&self, report: &ClientReport) {
        if let Err(error) = self.sender.try_send(report.clone()) {
            warn!(report_id = %report.report_id, "optional OTLP queue rejected a report: {error}");
        }
    }

    fn abort(self: std::sync::Arc<Self>) {
        // OTLP is best-effort and every primary report has already been
        // acknowledged before it reaches this queue. Do not let a Collector
        // timeout extend service shutdown by as much as 300 seconds.
        self.worker.abort();
    }
}
