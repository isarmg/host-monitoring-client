fn elevated_pair(server: String, callback_nonce: String) -> anyhow::Result<()> {
    ensure_process_is_elevated()?;
    let _pair_mutex = create_single_instance_mutex(PRIVILEGED_OPERATION_MUTEX, true)?
        .context("another host-monitor pairing operation is already running")?;
    let server = validate_server_base(&server)?;
    let origin = sarmg_client_secure_http::Url::parse(&server)?
        .origin()
        .ascii_serialization();
    let confirmation = format!(
        "即将把此 Windows 设备配对到：\n\n{origin}\n\n配对成功会替换当前本地绑定，并在 Server 中创建新的主机实例。只有确认该地址属于你的 Host Monitoring 服务器时才继续。",
    );
    match message_box(
        &confirmation,
        "确认 host-monitor 配对",
        MB_OKCANCEL | MB_ICONWARNING,
    ) {
        result if result == IDOK.0 => {}
        result if result == IDCANCEL.0 => return Ok(()),
        result => bail!("Windows could not display the pairing confirmation (result {result})"),
    }

    let original_state = wait_for_stable_service_state(Duration::from_secs(30))?;
    ensure!(
        matches!(
            original_state,
            ServiceState::Running | ServiceState::Stopped
        ),
        "Client service is in unsupported state {}",
        original_state.label()
    );
    let outcome = run_hidden_pair(&server, &callback_nonce);
    match outcome {
        Ok(()) => {
            if let Some(warning) = super::committed_pairing_restart_warning(
                original_state == ServiceState::Running,
                restart_service,
            ) {
                message_box(
                    &warning,
                    "host-monitor 配对已完成（服务需要处理）",
                    MB_OK | MB_ICONWARNING,
                );
            }
            Ok(())
        }
        Err(error) => Err(error.context(format!(
            "配对未完成；Client 服务保持操作前的状态（{}）",
            original_state.label()
        ))),
    }
}

fn elevated_service(action: ServiceAction, notify: bool) -> anyhow::Result<()> {
    ensure_process_is_elevated()?;
    let _operation = create_single_instance_mutex(PRIVILEGED_OPERATION_MUTEX, true)?
        .context("another host-monitor pairing or service operation is already running")?;
    match action {
        ServiceAction::Start => start_service()?,
        ServiceAction::Stop => stop_service()?,
    }
    if notify {
        message_box(
            match action {
                ServiceAction::Start => "host-monitor 服务已启动。",
                ServiceAction::Stop => "host-monitor 服务已停止；Windows 下次启动时仍会自动运行。",
            },
            "host-monitor",
            MB_OK | MB_ICONINFORMATION,
        );
    }
    Ok(())
}

fn elevated_stop_for_exit() -> anyhow::Result<()> {
    ensure_process_is_elevated()?;
    let _operation = create_single_instance_mutex(PRIVILEGED_OPERATION_MUTEX, true)?
        .context("another host-monitor pairing or service operation is already running")?;
    stop_service()?;
    ensure!(
        query_service_state()? == ServiceState::Stopped,
        "Client service did not remain stopped"
    );
    Ok(())
}

fn run_hidden_pair(server: &str, callback_nonce: &str) -> anyhow::Result<()> {
    let client = installed_program_root()?.join(CLIENT_EXE);
    let state_root = installed_state_root()?;
    let config = state_root.join(CONFIG_FILE);
    ensure_fixed_regular_file(&client, "installed Client executable")?;
    ensure!(
        state_root.is_dir(),
        "installed Client state directory is missing"
    );
    if config.exists() {
        ensure_fixed_regular_file(&config, "installed Client configuration")?;
    }
    let cancel_event_name = format!("Local\\HostMonitorPairCancel-{callback_nonce}");
    let cancel_event_name_wide = wide(&cancel_event_name);
    let cancel_event =
        unsafe { CreateEventW(None, true, false, PCWSTR(cancel_event_name_wide.as_ptr())) }
            .context("failed to create the pairing cancellation event")?;
    let cancel_event = KernelHandle(cancel_event);
    let mut command = Command::new(&client);
    command
        .arg("pair")
        .arg("--tray-events")
        .arg("--tray-activation-stdin")
        .arg("--config")
        .arg(&config)
        .arg("--server")
        .arg(server)
        .arg("--tray-cancel-event")
        .arg(&cancel_event_name)
        .arg("--tray-deadline-seconds")
        .arg(PAIR_OPERATION_TIMEOUT.as_secs().to_string())
        .arg("--replace-pending-pairing");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    set_clean_client_environment(&mut command)?;
    let child = command
        .spawn()
        .with_context(|| format!("failed to start {}", client.display()))?;
    let mut child = PairChildGuard::new(child, cancel_event.0);
    let stdin = child
        .child_mut()
        .stdin
        .take()
        .context("pairing stdin pipe is unavailable")?;
    let stdout = child
        .child_mut()
        .stdout
        .take()
        .context("pairing stdout pipe is unavailable")?;
    let stderr = child
        .child_mut()
        .stderr
        .take()
        .context("pairing stderr pipe is unavailable")?;
    let completion_pipe = Arc::new(Mutex::new(None));
    let completion_pipe_for_output = Arc::clone(&completion_pipe);
    let server_for_output = server.to_string();
    let nonce_for_output = callback_nonce.to_string();
    let mut stdout_thread = Some(
        thread::Builder::new()
            .name("host-monitoring-pair-stdout".into())
            .spawn(move || {
                process_pair_events(
                    stdout,
                    stdin,
                    &server_for_output,
                    &nonce_for_output,
                    &completion_pipe_for_output,
                )
            })
            .context("failed to start pairing event reader")?,
    );
    let stderr_thread = thread::Builder::new()
        .name("host-monitoring-pair-stderr".into())
        .spawn(move || drain_limited(stderr, MAX_CHILD_STDERR_BYTES))
        .context("failed to start pairing diagnostics reader")?;
    let deadline = Instant::now() + PAIR_OPERATION_TIMEOUT + Duration::from_secs(15);
    let mut completed_event_result = None;
    let mut cancellation_requested = false;
    let mut cancellation_requested_at = None;
    let mut forced_termination = false;
    let status = loop {
        if let Some(status) = child
            .child_mut()
            .try_wait()
            .context("failed to query browser pairing process")?
        {
            break status;
        }
        if stdout_thread
            .as_ref()
            .is_some_and(|reader| reader.is_finished())
        {
            let result = stdout_thread
                .take()
                .expect("checked above")
                .join()
                .map_err(|_| anyhow::anyhow!("pairing event reader panicked"))?;
            if result.is_err() && !cancellation_requested {
                unsafe { SetEvent(cancel_event.0) }
                    .context("failed to cancel pairing after an event-stream error")?;
                cancellation_requested = true;
                cancellation_requested_at = Some(Instant::now());
            }
            completed_event_result = Some(result);
        }
        if Instant::now() >= deadline && !cancellation_requested {
            unsafe { SetEvent(cancel_event.0) }
                .context("failed to signal the pairing safety deadline")?;
            cancellation_requested = true;
            cancellation_requested_at = Some(Instant::now());
        }
        if cancellation_requested_at
            .is_some_and(|requested| requested.elapsed() >= Duration::from_secs(30))
        {
            // The signed child normally observes the named event while its
            // in-flight reqwest future is safely dropped. Termination is a
            // final containment fallback only if that protocol fails for
            // 30 seconds; it is never the normal cancellation path.
            child
                .child_mut()
                .kill()
                .context("failed to contain an unresponsive pairing child")?;
            forced_termination = true;
            break child
                .child_mut()
                .wait()
                .context("failed to reap the contained pairing child")?;
        }
        thread::sleep(Duration::from_millis(100));
    };
    let event_result = if let Some(result) = completed_event_result {
        result
    } else {
        stdout_thread
            .take()
            .context("pairing event reader disappeared")?
            .join()
            .map_err(|_| anyhow::anyhow!("pairing event reader panicked"))?
    };
    let diagnostics = stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("pairing diagnostics reader panicked"))??;
    child.disarm();
    ensure!(
        !forced_termination,
        "pairing child ignored graceful cancellation and was contained after 30 seconds"
    );
    let reconciliation = super::reconcile_pairing_child(
        status.success(),
        &status.to_string(),
        event_result,
        &diagnostics,
    )?;
    let completion_pipe = completion_pipe
        .lock()
        .map_err(|_| anyhow::anyhow!("pairing completion pipe lock was poisoned"))?
        .take()
        .context("pairing completion pipe disappeared")?
        .into_kernel();
    let completion = serde_json::to_vec(&reconciliation)?;
    ensure!(
        completion.len() <= MAX_LOCAL_HTTP_BODY_BYTES,
        "pairing completion IPC message is too large"
    );
    write_pipe_frame(
        completion_pipe.0,
        &completion,
        Instant::now() + Duration::from_secs(10),
        None,
    )?;
    Ok(())
}

fn process_pair_events<R: Read, W: Write>(
    reader: R,
    writer: W,
    server: &str,
    callback_nonce: &str,
    completion_pipe: &Mutex<Option<TransferHandle>>,
) -> anyhow::Result<()> {
    let mut reader = BufReader::with_capacity(4096, reader);
    let mut writer = Some(writer);
    let mut line = Vec::new();
    let mut total = 0_usize;
    let mut activation_sent = false;
    let mut waiting_request_id = None;
    let mut paired = false;
    loop {
        line.clear();
        let read = read_bounded_line(&mut reader, &mut line, MAX_NDJSON_LINE_BYTES)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read);
        ensure!(
            total <= MAX_NDJSON_TOTAL_BYTES,
            "pairing event stream is too large"
        );
        while line
            .last()
            .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
        {
            line.pop();
        }
        ensure!(
            !line.is_empty(),
            "pairing event stream contains an empty line"
        );
        let event: PairEvent =
            serde_json::from_slice(&line).context("Client emitted invalid pairing NDJSON")?;
        match event {
            PairEvent::PairingWaiting {
                _version: _,
                generation,
                request_id,
                activation_url,
                pairing_endpoint,
                _expires_at: _,
                _poll_interval: _,
            } => {
                ensure!(
                    !activation_sent,
                    "Client emitted more than one activation event"
                );
                let activation_url = validate_browser_url(&activation_url)?;
                ensure!(
                    browser_url_matches_server_origin(&activation_url, server),
                    "Client activation URL origin differs from the confirmed server"
                );
                canonical_uuid(&request_id, "Client pairing request id")?;
                canonical_uuid(&generation, "Client pairing generation")?;
                let (activation_code, pipe) = exchange_pairing_code(
                    callback_nonce,
                    &PairIpcMessage {
                        generation,
                        request_id: request_id.clone(),
                        activation_url,
                        pairing_endpoint,
                    },
                )?;
                let mut retained_pipe = completion_pipe
                    .lock()
                    .map_err(|_| anyhow::anyhow!("pairing completion pipe lock was poisoned"))?;
                ensure!(
                    retained_pipe.is_none(),
                    "pairing completion pipe was already retained"
                );
                *retained_pipe = Some(pipe);
                drop(retained_pipe);
                let mut input = writer
                    .take()
                    .context("authorization-key stdin was already consumed")?;
                input.write_all(activation_code.as_bytes())?;
                input.write_all(b"\n")?;
                input.flush()?;
                drop(input);
                drop(activation_code);
                activation_sent = true;
                waiting_request_id = Some(request_id);
            }
            PairEvent::Paired {
                _version: _,
                request_id,
                instance_id,
                _endpoint: _,
            } => {
                ensure!(
                    activation_sent,
                    "Client paired before the authorization key was supplied"
                );
                ensure!(!paired, "Client emitted more than one paired event");
                ensure!(
                    Some(request_id.as_str()) == waiting_request_id.as_deref(),
                    "Client paired a different request than the one authorized"
                );
                canonical_uuid(&request_id, "Client paired request id")?;
                canonical_uuid(&instance_id, "Client paired instance id")?;
                paired = true;
            }
            PairEvent::PairingInterrupted {
                _version: _,
                _request_id,
            } => {
                canonical_uuid(&_request_id, "Client interrupted request id")?;
                bail!("browser pairing was interrupted");
            }
            PairEvent::PairingCancelled { _version: _ } => {
                bail!("browser pairing was cancelled safely")
            }
            PairEvent::PairingTimeout { _version: _ } => {
                bail!("browser pairing reached its safety deadline")
            }
        }
    }
    if !activation_sent {
        return Err(super::MissingAuthorizationKeyEvent.into());
    }
    ensure!(paired, "Client did not emit a successful pairing event");
    Ok(())
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    limit: usize,
) -> anyhow::Result<usize> {
    let mut read = 0_usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(read);
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(available.len());
        ensure!(
            line.len().saturating_add(take) <= limit,
            "pairing event line is too large"
        );
        let ends_line = available.get(take - 1) == Some(&b'\n');
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        read += take;
        if ends_line {
            return Ok(read);
        }
    }
}

fn drain_limited<R: Read>(mut reader: R, limit: usize) -> anyhow::Result<String> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
        truncated |= read > remaining;
    }
    let mut diagnostics = String::from_utf8_lossy(&retained).to_string();
    if truncated {
        diagnostics.push_str("\n[diagnostics truncated]");
    }
    Ok(diagnostics)
}

fn exchange_pairing_code(
    nonce: &str,
    message: &PairIpcMessage,
) -> anyhow::Result<(SensitiveActivationCode, TransferHandle)> {
    validate_callback_nonce(nonce)?;
    let pipe_name = pair_pipe_name(nonce);
    let pipe_name_wide = wide(&pipe_name);
    let deadline = Instant::now() + Duration::from_secs(10);
    let pipe = loop {
        match unsafe {
            CreateFileW(
                PCWSTR(pipe_name_wide.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE::default(),
                None,
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION | FILE_FLAG_OVERLAPPED,
                None,
            )
        } {
            Ok(pipe) => break KernelHandle(pipe),
            Err(_) => match unsafe { GetLastError() } {
                ERROR_PIPE_BUSY | ERROR_FILE_NOT_FOUND => {
                    let wait_millis = deadline_wait_millis(Instant::now(), deadline)
                        .context("timed out connecting to the protected tray pipe")?
                        .min(200);
                    let _ = unsafe { WaitNamedPipeW(PCWSTR(pipe_name_wide.as_ptr()), wait_millis) };
                }
                error => bail!("could not connect to the protected tray pipe: {error:?}"),
            },
        }
    };
    let body = serde_json::to_vec(message)?;
    ensure!(
        body.len() <= MAX_LOCAL_HTTP_BODY_BYTES,
        "pairing IPC message is too large"
    );
    write_pipe_frame(pipe.0, &body, deadline, None)?;
    let code = read_pipe_frame(pipe.0, 256, deadline, None)?;
    let code = String::from_utf8(code).context("tray supplied a non-UTF-8 authorization key")?;
    validate_activation_code(&code)?;
    Ok((
        SensitiveActivationCode::new(code),
        TransferHandle::new(pipe),
    ))
}

fn validate_pair_ipc_message(message: &PairIpcMessage, server: &str) -> anyhow::Result<()> {
    canonical_uuid(&message.generation, "pairing IPC generation")?;
    let request_id = canonical_uuid(&message.request_id, "pairing IPC request id")?;
    let expected_pairing_endpoint = format!(
        "{}{}",
        server.trim_end_matches('/'),
        host_protocol::CLIENT_PAIRING_REQUESTS_PATH
    );
    ensure!(
        message.pairing_endpoint == expected_pairing_endpoint,
        "Client pairing endpoint differs from the confirmed server"
    );
    let pairing_endpoint = validate_browser_url(&message.pairing_endpoint)?;
    ensure!(
        browser_url_matches_server_origin(&pairing_endpoint, server),
        "Client pairing endpoint origin differs from the confirmed server"
    );
    let activation_url = validate_browser_url(&message.activation_url)?;
    let activation = sarmg_client_secure_http::Url::parse(&activation_url)?;
    ensure!(
        browser_url_matches_server_origin(&activation_url, &pairing_endpoint),
        "Client activation URL origin differs from its pairing endpoint"
    );
    ensure!(
        activation.username().is_empty()
            && activation.password().is_none()
            && activation.query().is_none()
            && activation.fragment().is_none()
            && activation.path()
                == format!(
                    "{}{request_id}",
                    host_protocol::BROWSER_ACTIVATION_PATH_PREFIX
                ),
        "Client activation URL does not match its pairing request id"
    );
    Ok(())
}
