/// Health tracking for one category of spool disk operation.
///
/// Continue sampling and delivery after isolated I/O failures. Exit only when consecutive failures in the same category reach the threshold,
/// delegating persistent faults to the service manager. The main loop maintains separate instances for
/// reads, writes and backlog delivery so a successful read cannot mask persistent write failures.
#[derive(Default)]
struct SpoolHealth {
    failures: xcsc::runtime::QueueFailureStreak,
}

impl SpoolHealth {
    fn record_success(&mut self) {
        self.failures.record_success();
    }

    /// Record a failure. Return `Err` and terminate the main loop only when consecutive failures reach the threshold.
    fn record_failure(
        &mut self,
        operation: &str,
        error: &dyn std::fmt::Display,
    ) -> anyhow::Result<()> {
        let outcome = self.failures.record_failure();
        warn!(
            event = "xsoc.queue.persistence_failed", error_code = "queue_io_failed", operation = operation, error = %error,
            consecutive_failures = self.failures.consecutive_failures(),
            "{operation} failed; continuing in degraded mode: {error}"
        );
        outcome.context("persistent spool failure; exiting for service-manager recovery")
    }

    /// Attempt to write a report to spool. On failure, drop the report and continue instead of terminating the process.
    fn try_enqueue(&mut self, spool: &Spool, report: &ClientReport) -> anyhow::Result<()> {
        match spool.enqueue(report) {
            Ok(()) => {
                self.record_success();
                Ok(())
            }
            Err(error) => {
                self.record_failure("writing spool", &error)?;
                warn!(event = "xsoc.collection.discarded", instance_id = %report.host.id, request_id = %report.report_id, error_code = "queue_io_failed", "sample could not be persisted");
                Ok(())
            }
        }
    }
}

type FlushOutcome = xcsc::runtime::BatchOutcome<xsoc::transport::SendError>;

struct HostDeliveryAdapter<'a> {
    reporter: &'a Reporter,
    otlp_queue: Option<&'a OtlpQueue>,
}

impl xcsc::runtime::DeliveryAdapter<xsoc::spool::PendingReport>
    for HostDeliveryAdapter<'_>
{
    type Error = xsoc::transport::SendError;

    async fn send(&self, pending: &xsoc::spool::PendingReport) -> Result<(), Self::Error> {
        self.reporter
            .send_queued_xsos(&pending.report, &pending.body)
            .await
    }

    fn disposition(&self, error: &Self::Error) -> xcsc::runtime::FailureDisposition {
        use xcsc::runtime::{FailureDisposition, QuarantineReason};
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
            "spooled report permanently rejected and discarded: {error}"
        );
    }
    fn quarantined(&self, pending: &xsoc::spool::PendingReport, reason: xcsc::runtime::QuarantineReason) {
        warn!(event = "xsoc.queue.isolated", instance_id = %pending.report.host.id, request_id = %pending.report.report_id, ?reason, "spool record isolated with original bytes preserved; inspect status/doctor");
    }
}

async fn flush_spool(
    spool: &Spool,
    reporter: &Reporter,
    otlp_queue: Option<&OtlpQueue>,
) -> anyhow::Result<FlushOutcome> {
    xcsc::runtime::deliver_batch(
        spool,
        &HostDeliveryAdapter {
            reporter,
            otlp_queue,
        },
    )
    .await
}
