/// 单类 spool 磁盘操作的健康度跟踪。
///
/// 单次 I/O 失败（磁盘瞬时写满、目录被误删、权限被改动）不应终止一个常驻守护进程：
/// 退出只会表现为反复崩溃重启，且期间连内存直传都停了。这里改为降级续跑，只有在
/// **同类操作连续**失败到阈值时才退出，把持续性故障交给服务管理器处理。主循环为
/// 读、写和补传各持有一个实例，避免“读取成功”掩盖“持续不可写”。
#[derive(Default)]
struct SpoolHealth {
    failures: sarmg_client_runtime::QueueFailureStreak,
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
                warn!(report_id = %report.report_id, "本次采样未能持久化，已丢弃");
                Ok(())
            }
        }
    }
}

type FlushOutcome = sarmg_client_runtime::BatchOutcome<host_monitor::transport::SendError>;

struct HostDeliveryAdapter<'a> {
    reporter: &'a Reporter,
    otlp_queue: Option<&'a OtlpQueue>,
}

impl sarmg_client_runtime::DeliveryAdapter<host_monitor::spool::PendingReport>
    for HostDeliveryAdapter<'_>
{
    type Error = host_monitor::transport::SendError;

    async fn send(&self, pending: &host_monitor::spool::PendingReport) -> Result<(), Self::Error> {
        self.reporter.send_host_monitoring(&pending.report).await
    }

    fn disposition(&self, error: &Self::Error) -> sarmg_client_runtime::FailureDisposition {
        use sarmg_client_runtime::{FailureDisposition, QuarantineReason};
        match error {
            host_monitor::transport::SendError::IdentityMismatch => FailureDisposition::Quarantine(QuarantineReason::IdentityMismatch),
            error if error.is_permanent() => FailureDisposition::Discard,
            _ => FailureDisposition::Retain,
        }
    }

    fn acknowledged(&self, pending: &host_monitor::spool::PendingReport) {
        if let Some(queue) = self.otlp_queue {
            queue.try_export(&pending.report);
        }
    }

    fn discarded(&self, pending: &host_monitor::spool::PendingReport, error: &Self::Error) {
        error!(
            report_id = %pending.report.report_id,
            "spool 中的报文被永久拒绝，已丢弃：{error}"
        );
    }
    fn quarantined(&self, _pending: &host_monitor::spool::PendingReport, reason: sarmg_client_runtime::QuarantineReason) {
        warn!(?reason, "spool record isolated with original bytes preserved; inspect status/doctor");
    }
}

async fn flush_spool(
    spool: &Spool,
    reporter: &Reporter,
    otlp_queue: Option<&OtlpQueue>,
) -> anyhow::Result<FlushOutcome> {
    sarmg_client_runtime::deliver_batch(
        spool,
        &HostDeliveryAdapter {
            reporter,
            otlp_queue,
        },
    )
    .await
}
