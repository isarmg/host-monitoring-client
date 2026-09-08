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
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let endpoint = format!("{origin}/api/v2/host-monitor/report");
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
                "version": "0.9.4", "status": "authorized",
                "reason": "fixture", "changed_at": chrono::Utc::now(),
            }))
            .unwrap(),
        );
        write_private(
            "pairing-state.json",
            &serde_json::to_vec(&serde_json::json!({
                "phase": "active", "version": "0.9.4",
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
                "version": "0.9.4", "generation": generation,
                "request_id": request_id, "instance_id": instance_id, "report_endpoint": endpoint,
            }))
            .unwrap(),
        );
        let mut config = ClientConfig::default();
        config.endpoint = endpoint;
        config.state_dir = directory.0.clone();
        config.jitter_percent = 0;
        config.request_timeout_seconds = 3;
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
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let count = stream.read(&mut chunk).unwrap();
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
        let (wake, receiver) = sarmg_client_runtime::DeliveryWake::channel();
        let (stop, shutdown) = watch::channel(false);
        let (host_updates, _host_receiver) = watch::channel(host.clone());
        let driver = HostDeliveryDriver::new(config, host, spool.clone(), reporter, host_updates);
        let task = tokio::spawn(
            sarmg_client_runtime::DeliveryWorker::new(driver, 0)
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
        let (trigger, _receiver) = sarmg_client_runtime::DeliveryWake::channel();
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
            std::env::temp_dir().canonicalize().expect("physical test temporary directory").join(format!("host-monitoring-once-shutdown-{}", Uuid::new_v4()));
        let spool = Spool::open(&directory, 1024 * 1024).unwrap();
        let mut sampler = SystemSampler::new();
        let report = sampler.collect(transient_host_identity(Uuid::new_v4()), 10, 0);
        let (controller, shutdown) = host_monitor::service::shutdown_channel();
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

    /// 偶发 I/O 失败必须降级续跑，不能终止常驻进程。
    #[test]
    fn transient_spool_failures_do_not_stop_the_client() {
        let mut health = SpoolHealth::default();
        for _ in 0..(sarmg_client_runtime::MAX_QUEUE_FAILURES - 1) {
            health
                .record_failure("测试", &"disk full")
                .expect("未达阈值前必须继续运行");
        }
    }

    /// 但持续性故障要退出，交给服务管理器处理——否则会静默地一直丢数据。
    #[test]
    fn sustained_spool_failures_eventually_stop_the_client() {
        let mut health = SpoolHealth::default();
        for _ in 0..(sarmg_client_runtime::MAX_QUEUE_FAILURES - 1) {
            health.record_failure("测试", &"disk full").unwrap();
        }
        let error = health
            .record_failure("测试", &"disk full")
            .expect_err("达到阈值必须返回错误以终止主循环");
        assert!(
            error.to_string().contains("持续性故障"),
            "错误信息应说明这是持续性故障而非偶发，实际为：{error}"
        );
    }

    /// 中间只要成功一次，计数就归零——阈值针对的是**连续**失败。
    #[test]
    fn a_single_success_resets_the_failure_streak() {
        let mut health = SpoolHealth::default();
        for _ in 0..(sarmg_client_runtime::MAX_QUEUE_FAILURES - 1) {
            health.record_failure("测试", &"transient").unwrap();
        }
        health.record_success();
        // 归零后应能再撑满一整轮，说明计数确实被重置了。
        for _ in 0..(sarmg_client_runtime::MAX_QUEUE_FAILURES - 1) {
            health
                .record_failure("测试", &"transient")
                .expect("成功一次后计数应归零");
        }
    }
}
