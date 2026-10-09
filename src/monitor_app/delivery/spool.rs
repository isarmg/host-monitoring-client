/// 单类 spool 磁盘操作的健康度跟踪。
///
/// 单次 I/O 失败时降级继续采样与投递；只有在**同类操作连续**失败到阈值时才退出，
/// 把持续性故障交给服务管理器处理。主循环为
/// 读、写和补传各持有一个实例，避免“读取成功”掩盖“持续不可写”。
#[derive(Default)]
struct SpoolHealth {
    failures: xcsc_runtime::QueueFailureStreak,
}

impl SpoolHealth {
    fn record_success(&mut self) {
        self.failures.record_success();
    }

    /// 记录一次失败。仅当连续失败达到阈值时才返回 `Err`（从而终止主循环）。
    fn record_failure(
        &mut self,
        operation: &str,
        error: &dyn std::fmt::Display,
    ) -> anyhow::Result<()> {
        let outcome = self.failures.record_failure();
        warn!(
            event = "xsoc.queue.persistence_failed", error_code = "queue_io_failed", operation = operation, error = %error,
            consecutive_failures = self.failures.consecutive_failures(),
            "{operation}失败，已降级继续运行：{error}"
        );
        outcome.context("spool 持续性故障；退出并交由服务管理器处理")
    }

    /// 尝试把报文写入 spool。写不进去时丢弃该报文并继续，而不是终止进程。
    fn try_enqueue(&mut self, spool: &Spool, report: &ClientReport) -> anyhow::Result<()> {
        match spool.enqueue(report) {
            Ok(()) => {
                self.record_success();
                Ok(())
            }
            Err(error) => {
                self.record_failure("写入 spool", &error)?;
                warn!(event = "xsoc.collection.discarded", instance_id = %report.host.id, request_id = %report.report_id, error_code = "queue_io_failed", "sample could not be persisted");
                Ok(())
            }
        }
    }
}

type FlushOutcome = xcsc_runtime::BatchOutcome<xsoc::transport::SendError>;

struct HostDeliveryAdapter<'a> {
    reporter: &'a Reporter,
    otlp_queue: Option<&'a OtlpQueue>,
}

impl xcsc_runtime::DeliveryAdapter<xsoc::spool::PendingReport>
    for HostDeliveryAdapter<'_>
{
    type Error = xsoc::transport::SendError;

    async fn send(&self, pending: &xsoc::spool::PendingReport) -> Result<(), Self::Error> {
        self.reporter
            .send_queued_xsos(&pending.report, &pending.body)
            .await
    }

    fn disposition(&self, error: &Self::Error) -> xcsc_runtime::FailureDisposition {
        use xcsc_runtime::{FailureDisposition, QuarantineReason};
        match error {
            xsoc::transport::SendError::IdentityMismatch => FailureDisposition::Quarantine(QuarantineReason::IdentityMismatch),
            error if error.is_permanent() => FailureDisposition::Discard,
            _ => FailureDisposition::Retain,
        }
    }

    fn acknowledged(&self, pending: &xsoc::spool::PendingReport) {
        if let Some(queue) = self.otlp_queue {
            queue.try_export(&pending.report);
        }
    }

    fn discarded(&self, pending: &xsoc::spool::PendingReport, error: &Self::Error) {
        error!(
            event = "xsoc.delivery.rejected", instance_id = %pending.report.host.id, request_id = %pending.report.report_id, error_code = %error.stable_code().to_ascii_lowercase(),
            "spool 中的报文被永久拒绝，已丢弃：{error}"
        );
    }
    fn quarantined(&self, pending: &xsoc::spool::PendingReport, reason: xcsc_runtime::QuarantineReason) {
        warn!(event = "xsoc.queue.isolated", instance_id = %pending.report.host.id, request_id = %pending.report.report_id, ?reason, "spool record isolated with original bytes preserved; inspect status/doctor");
    }
}

async fn flush_spool(
    spool: &Spool,
    reporter: &Reporter,
    otlp_queue: Option<&OtlpQueue>,
) -> anyhow::Result<FlushOutcome> {
    xcsc_runtime::deliver_batch(
        spool,
        &HostDeliveryAdapter {
            reporter,
            otlp_queue,
        },
    )
    .await
}
