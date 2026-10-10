#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises the real coordinator, Reporter HTTP request and durable Spool,
    /// not a look-alike select loop. Wake edges must not cancel a slow response.
    #[cfg(unix)]
    #[tokio::test]
    async fn sampling_wakes_do_not_cancel_an_in_flight_http_batch() {
        use std::os::unix::fs::PermissionsExt;
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let directory = Directory(
            std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!("host-delivery-flight-{}", Uuid::new_v4())),
        );
        fs::create_dir(&directory.0).unwrap();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
        let https = crate::test_https::TestHttpsServer::new();
        let origin = https.origin.clone();
        let endpoint = format!("{origin}/api/v1/xsoc/report");
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let write_private = |name: &str, body: &[u8]| {
            let path = directory.0.join(name);
            fs::write(&path, body).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        };
        write_private("host-id", instance_id.to_string().as_bytes());
        write_private("client-token", "a".repeat(64).as_bytes());
        write_private(
            "auth-state.json",
            &serde_json::to_vec(&serde_json::json!({
                "version": "1.0.0", "status": "authorized",
                "reason": "fixture", "changed_at": chrono::Utc::now(),
            }))
            .unwrap(),
        );
        write_private(
            "pairing-state.json",
            &serde_json::to_vec(&serde_json::json!({
                "phase": "active", "version": "1.0.0",
                "generation": generation, "request_id": request_id,
                "activation_url": format!("{origin}/activate/test"),
                "instance_id": instance_id, "report_endpoint": endpoint,
                "completed_at": chrono::Utc::now(),
            }))
            .unwrap(),
        );
        write_private(
            "active-binding.json",
            &serde_json::to_vec(&serde_json::json!({
                "version": "1.0.0", "generation": generation,
                "request_id": request_id, "instance_id": instance_id, "report_endpoint": endpoint,
            }))
            .unwrap(),
        );
        let mut config = ClientConfig::default();
        config.endpoint = endpoint;
        config.state_dir = directory.0.clone();
        config.jitter_percent = 0;
        config.request_timeout_seconds = 3;
        config.tls_ca_pem = Some(https.ca_path.clone());
        let reporter = Reporter::new(&config).unwrap();
        let host = load_host_identity(&directory.0).unwrap();
        let spool = Spool::open(&directory.0, 1024 * 1024).unwrap();
        let report = SystemSampler::new().collect(host.clone(), 10, 0);
        spool.enqueue(&report).unwrap();
        let acknowledgement = serde_json::to_vec(&serde_json::json!({
            "host_id": report.host.id, "report_id": report.report_id,
            "accepted": true, "received_at": chrono::Utc::now(),
        }))
        .unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut stream = https.accept();
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            let read_deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let count = match stream.read(&mut chunk) {
                    Ok(count) => count,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) && Instant::now() < read_deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("fixture request read: {error}"),
                };
                assert!(count > 0 && request.len() + count <= 1024 * 1024);
                request.extend_from_slice(&chunk[..count]);
                if let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                {
                    let length: usize = std::str::from_utf8(&request[..header_end])
                        .unwrap()
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if request.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            write!(stream, "HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", acknowledgement.len()).unwrap();
            stream.write_all(&acknowledgement).unwrap();
        });
        let (wake, receiver) = xcsc::runtime::DeliveryWake::channel();
        let (stop, shutdown) = watch::channel(false);
        let (host_updates, _host_receiver) = watch::channel(host.clone());
        let driver = HostDeliveryDriver::new(config, host, spool.clone(), reporter, host_updates);
        let task = tokio::spawn(
            xcsc::runtime::DeliveryWorker::new(driver, 0)
                .unwrap()
                .run(receiver, shutdown),
        );
        tokio::time::timeout(Duration::from_secs(3), started_rx)
            .await
            .unwrap()
            .unwrap();
        for _ in 0..10 {
            assert!(wake.notify());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        release_tx.send(()).unwrap();
        let drained = tokio::time::timeout(Duration::from_secs(2), async {
            while spool.pending_count().unwrap() != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.join().unwrap();
        assert!(
            drained.is_ok(),
            "sampling wake cancelled the only successful HTTP response"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_delivery_notification_channel_does_not_shift_sampling_cadence() {
        let (trigger, _receiver) = xcsc::runtime::DeliveryWake::channel();
        let mut cadence = SamplingCadence::starting_now();
        let start = cadence.deadline();

        for index in 0..4 {
            tokio::time::sleep_until(cadence.deadline()).await;
            assert_eq!(
                tokio::time::Instant::now(),
                start + Duration::from_secs(index * 10),
                "a blocked delivery consumer must not move sampling tick {index}"
            );
            assert!(trigger.notify());
            cadence.schedule_next(Duration::from_secs(10), tokio::time::Instant::now());
        }
    }

    #[test]
    fn cadence_skips_an_overrun_instead_of_bursting_missed_samples() {
        let mut cadence = SamplingCadence::starting_now();
        let start = cadence.deadline();
        cadence.schedule_next(Duration::from_secs(10), start + Duration::from_secs(25));
        assert_eq!(cadence.deadline(), start + Duration::from_secs(35));
    }

    #[test]
    fn first_run_report_advertises_the_next_sampling_cadence() {
        let mut sampler = SystemSampler::new();
        let mut report = sampler.collect(transient_host_identity(Uuid::new_v4()), 3600, 0);
        let measured_network_rates: Vec<_> = report.system.networks.iter()
            .map(|network| (network.received_bytes_per_second, network.transmitted_bytes_per_second))
            .collect();
        let measured_disk_rates: Vec<_> = report.system.disks.iter()
            .map(|disk| (disk.read_bytes_per_second, disk.written_bytes_per_second))
            .collect();
        let mut first_report = true;

        advertise_first_sampling_cadence(&mut report, Duration::from_secs(3600), &mut first_report);
        assert_eq!(report.interval_seconds, 3600.0);
        assert!(!first_report);
        assert_eq!(report.system.networks.iter()
            .map(|network| (network.received_bytes_per_second, network.transmitted_bytes_per_second))
            .collect::<Vec<_>>(), measured_network_rates);
        assert_eq!(report.system.disks.iter()
            .map(|disk| (disk.read_bytes_per_second, disk.written_bytes_per_second))
            .collect::<Vec<_>>(), measured_disk_rates);

        report.interval_seconds = 25.0;
        advertise_first_sampling_cadence(&mut report, Duration::from_secs(3600), &mut first_report);
        assert_eq!(report.interval_seconds, 25.0);
    }

    #[tokio::test(start_paused = true)]
    async fn delivery_worker_shutdown_has_a_hard_upper_bound() {
        let worker = tokio::spawn(async {
            std::future::pending::<()>().await;
            Ok(())
        });
        let started = tokio::time::Instant::now();
        stop_delivery_worker(worker).await.unwrap();
        assert_eq!(
            tokio::time::Instant::now().duration_since(started),
            Duration::from_secs(5)
        );
    }

    #[tokio::test]
    async fn cancelled_one_shot_retains_the_current_report_for_idempotent_retry() {
        let directory =
            std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!("xsos-once-shutdown-{}", Uuid::new_v4()));
        let spool = Spool::open(&directory, 1024 * 1024).unwrap();
        let mut sampler = SystemSampler::new();
        let report = sampler.collect(transient_host_identity(Uuid::new_v4()), 10, 0);
        let (controller, shutdown) = xsoc::service::shutdown_channel();
        controller.request_shutdown();

        let operation = finish_before_shutdown(&shutdown, std::future::pending::<()>()).await;
        assert!(operation.is_none());
        assert_eq!(
            retain_once_report(&spool, &report).unwrap(),
            RunOnceOutcome::Shutdown
        );
        let pending = spool
            .oldest()
            .unwrap()
            .expect("cancelled report is durable");
        assert_eq!(pending.report.report_id, report.report_id);

        drop(spool);
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Isolated I/O failures must degrade gracefully without terminating the resident process.
    #[test]
    fn transient_spool_failures_do_not_stop_the_client() {
        let mut health = SpoolHealth::default();
        for _ in 0..(xcsc::runtime::MAX_QUEUE_FAILURES - 1) {
            health
                .record_failure("test", &"disk full")
                .expect("continue running before the threshold is reached");
        }
    }

    /// Persistent faults must exit for service-manager recovery rather than silently discarding data forever.
    #[test]
    fn sustained_spool_failures_eventually_stop_the_client() {
        let mut health = SpoolHealth::default();
        for _ in 0..(xcsc::runtime::MAX_QUEUE_FAILURES - 1) {
            health.record_failure("test", &"disk full").unwrap();
        }
        let error = health
            .record_failure("test", &"disk full")
            .expect_err("return an error at the threshold to terminate the main loop");
        assert!(
            error.to_string().contains("persistent spool failure"),
            "the error must identify a persistent fault rather than an isolated failure; received: {error}"
        );
    }

    /// A successful operation resets the count; the threshold applies to consecutive failures.
    #[test]
    fn a_single_success_resets_the_failure_streak() {
        let mut health = SpoolHealth::default();
        for _ in 0..(xcsc::runtime::MAX_QUEUE_FAILURES - 1) {
            health.record_failure("test", &"transient").unwrap();
        }
        health.record_success();
        // After reset, another complete failure sequence must fit below the threshold.
        for _ in 0..(xcsc::runtime::MAX_QUEUE_FAILURES - 1) {
            health
                .record_failure("test", &"transient")
                .expect("one successful operation must reset the count");
        }
    }
}
