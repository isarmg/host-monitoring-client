fn route_request(
    request: LocalHttpRequest,
    origin: &str,
    state: &Arc<LocalControlState>,
) -> anyhow::Result<HttpResponse> {
    ensure!(
        request.headers.get("host").is_some_and(|host| {
            constant_time_eq(
                host.as_bytes(),
                origin.trim_start_matches("http://").as_bytes(),
            )
        }),
        "Host header does not match the loopback listener"
    );
    if request.method == "GET" && request.target == "/app.js" {
        ensure!(request.body.is_empty(), "GET request must not have a body");
        return Ok(response_javascript(APP_JAVASCRIPT));
    }
    if request.method == "POST" && request.target == "/session" {
        require_origin(&request, origin)?;
        require_json_request(&request)?;
        require_control_marker(&request)?;
        ensure!(
            request.body == b"{}",
            "session request body must be empty JSON"
        );
        let supplied = bearer(&request)?;
        let accepted = {
            let mut tokens = lock(&state.bootstrap_tokens);
            retain_valid_tokens(&mut tokens);
            tokens
                .iter()
                .position(|token| constant_time_eq(token.value.as_bytes(), supplied.as_bytes()))
                .map(|index| tokens.remove(index))
        };
        ensure!(accepted.is_some(), "invalid or expired browser capability");
        let session = BrowserSession {
            bearer: random_secret(),
            expires: Instant::now() + Duration::from_secs(8 * 60 * 60),
        };
        let bearer = session.bearer.clone();
        let mut sessions = lock(&state.sessions);
        sessions.retain(|session| session.expires > Instant::now());
        ensure!(
            sessions.len() < 32,
            "too many active local browser sessions"
        );
        sessions.push(session);
        return Ok(response_json(
            "200 OK",
            serde_json::json!({ "bearer": bearer }),
        ));
    }
    if request.method == "GET" && request.target == "/" {
        ensure!(request.body.is_empty(), "GET request must not have a body");
        return Ok(render_configuration());
    }
    if request.method == "POST" {
        require_origin(&request, origin)?;
        require_json_request(&request)?;
        require_control_marker(&request)?;
        authenticate_session(&request, state)?;
        return match request.target.as_str() {
            "/state" => local_state_response(&request.body, state),
            "/connection" => server_connection_response(&request.body, state),
            "/pair" => start_pair_from_browser(&request.body, state),
            "/service" => change_service_from_browser(&request.body, state),
            "/operation" => operation_response(&request.body, state),
            _ => bail!("unknown local-control route"),
        };
    }
    bail!("unknown local-control route")
}

fn operation_response(body: &[u8], state: &Arc<LocalControlState>) -> anyhow::Result<HttpResponse> {
    let request: OperationRequest =
        serde_json::from_slice(body).context("invalid operation request JSON")?;
    ensure!(
        request.id.len() == 64 && request.id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "operation id is invalid"
    );
    let operation = lock(&state.operations)
        .iter()
        .find(|operation| constant_time_eq(operation.id.as_bytes(), request.id.as_bytes()))
        .cloned()
        .context("operation was not found or has expired")?;
    Ok(response_json(
        "200 OK",
        serde_json::to_value(operation).context("failed to serialize operation status")?,
    ))
}

fn start_pair_from_browser(
    body: &[u8],
    state: &Arc<LocalControlState>,
) -> anyhow::Result<HttpResponse> {
    let request: PairRequest =
        serde_json::from_slice(body).context("invalid pairing request JSON")?;
    let server = validate_server_base(&request.server)?;
    validate_activation_code(&request.activation_code)?;
    let pairing_slot = claim_pairing_slot(state)?;
    let callback_nonce = random_secret();
    let ipc = PairIpcServer::create(&callback_nonce)?;
    let activation_code = SensitiveActivationCode::new(request.activation_code);
    let preferences_path = state.preferences_path.clone();
    let successful_preferences = TrayPreferences {
        application_version: CurrentPackageVersion,
        server: server.clone(),
    };
    let process = TransferHandle::new(launch_elevated_pair(&server, callback_nonce)?);
    let operation_id = create_operation(
        state,
        "pair",
        "awaiting_uac",
        "请确认 Windows 用户账户控制提示",
    );
    let worker_operation_id = operation_id.clone();
    let operation_state = Arc::clone(state);
    let server_for_ipc = server.clone();
    thread::Builder::new()
        .name("host-monitoring-pair-ipc".into())
        .spawn(move || {
            update_operation(
                &operation_state,
                &worker_operation_id,
                "pairing",
                "正在创建请求、提交授权并等待 Server 确认",
                None,
            );
            let outcome = ipc.serve(process, &server_for_ipc, activation_code);
            let preferences_warning = outcome.as_ref().ok().and_then(|_| {
                super::committed_pairing_preferences_warning(save_preferences(
                    &preferences_path,
                    &successful_preferences,
                ))
            });
            // The broker has now exited (or the bounded wait failed), so
            // release exclusivity before showing any user-dismissed UI.
            drop(pairing_slot);
            match outcome {
                Ok(outcome) => {
                    let service = query_service_state()
                        .map(|state| state.label().to_string())
                        .unwrap_or_else(|error| format!("无法查询：{error}"));
                    let mut warnings = Vec::new();
                    if let Some(warning) = outcome.post_commit_event_warning {
                        warnings.push(warning.to_string());
                    }
                    if let Some(warning) = preferences_warning {
                        warnings.push(warning);
                    }
                    let message = if warnings.is_empty() {
                        format!("配对成功；当前 Client 服务状态：{service}")
                    } else {
                        format!("{} 当前 Client 服务状态：{service}", warnings.join("\n\n"))
                    };
                    update_operation(
                        &operation_state,
                        &worker_operation_id,
                        "completed",
                        message,
                        Some(true),
                    );
                }
                Err(error) => {
                    let rendered = format!("{error:#}");
                    let cancelled = rendered
                        .contains("exited before requesting the authorization key (exit code 0)");
                    update_operation(
                        &operation_state,
                        &worker_operation_id,
                        if cancelled { "cancelled" } else { "failed" },
                        if cancelled {
                            "配对已由用户取消".to_string()
                        } else {
                            format!("配对失败：{error}")
                        },
                        Some(false),
                    );
                    if !cancelled {
                        show_error("host-monitor 配对失败；请在本地配置页面查看详情并重试。");
                    }
                }
            }
            notify_tray_status_changed();
        })
        .context("failed to start the protected pairing channel")?;
    Ok(response_json(
        "202 Accepted",
        serde_json::json!({
            "status": "elevation_requested",
            "operation_id": operation_id,
            "message": "请确认 Windows 用户账户控制提示；授权密钥将由托盘直接提交，配对会自动完成。"
        }),
    ))
}

fn change_service_from_browser(
    body: &[u8],
    state: &Arc<LocalControlState>,
) -> anyhow::Result<HttpResponse> {
    let request: ServiceRequest =
        serde_json::from_slice(body).context("invalid service request JSON")?;
    let action = match request.action.as_str() {
        "start" => ServiceAction::Start,
        "stop" => ServiceAction::Stop,
        _ => bail!("service action must be start or stop"),
    };
    let operation_slot = claim_service_operation(state)?;
    let process = TransferHandle::new(
        launch_elevated_process(
            &[
                "--elevated-service-browser".into(),
                match action {
                    ServiceAction::Start => "start".into(),
                    ServiceAction::Stop => "stop".into(),
                },
            ],
            true,
        )?
        .context("Windows did not return the elevated service process handle")?,
    );
    let operation_id = create_operation(
        state,
        "service",
        "awaiting_uac",
        "请确认 Windows 用户账户控制提示",
    );
    let worker_operation_id = operation_id.clone();
    let operation_state = Arc::clone(state);
    thread::Builder::new()
        .name("host-monitoring-browser-service-operation".into())
        .spawn(move || {
            update_operation(
                &operation_state,
                &worker_operation_id,
                "changing_service",
                match action {
                    ServiceAction::Start => "正在启动 Client 服务",
                    ServiceAction::Stop => "正在停止 Client 服务",
                },
                None,
            );
            let outcome = wait_for_elevated_service_action(process, action);
            drop(operation_slot);
            match outcome {
                Ok(()) => update_operation(
                    &operation_state,
                    &worker_operation_id,
                    "completed",
                    match action {
                        ServiceAction::Start => "Client 服务已启动",
                        ServiceAction::Stop => "Client 服务已停止；下次开机仍会自动启动",
                    },
                    Some(true),
                ),
                Err(error) => update_operation(
                    &operation_state,
                    &worker_operation_id,
                    "failed",
                    format!("服务操作失败：{error}"),
                    Some(false),
                ),
            }
            notify_tray_status_changed();
        })
        .context("failed to start the browser service-operation waiter")?;
    Ok(response_json(
        "202 Accepted",
        serde_json::json!({
            "status": "elevation_requested",
            "operation_id": operation_id,
            "message": "请确认 Windows 用户账户控制提示。"
        }),
    ))
}

fn authenticate_session(
    request: &LocalHttpRequest,
    state: &Arc<LocalControlState>,
) -> anyhow::Result<()> {
    let supplied = bearer(request)?;
    let mut sessions = lock(&state.sessions);
    sessions.retain(|session| session.expires > Instant::now());
    ensure!(
        sessions
            .iter()
            .any(|session| constant_time_eq(session.bearer.as_bytes(), supplied.as_bytes())),
        "browser session is missing or expired"
    );
    Ok(())
}

fn local_state_response(
    body: &[u8],
    state: &Arc<LocalControlState>,
) -> anyhow::Result<HttpResponse> {
    let _: StateRequest = serde_json::from_slice(body).context("invalid state request JSON")?;
    let preferences = load_preferences(&state.preferences_path).unwrap_or_default();
    let service_state = query_service_state();
    let service = service_state
        .as_ref()
        .map(|state| state.label().to_string())
        .unwrap_or_else(|error| format!("不可用：{error}"));
    let service_code = service_state
        .map(ServiceState::code)
        .unwrap_or("unavailable");
    Ok(response_json(
        "200 OK",
        serde_json::json!({
            "service": service,
            "service_code": service_code,
            "server": preferences.server,
            "version": env!("CARGO_PKG_VERSION")
        }),
    ))
}

fn server_connection_response(
    body: &[u8],
    state: &Arc<LocalControlState>,
) -> anyhow::Result<HttpResponse> {
    let request: ConnectionRequest =
        serde_json::from_slice(body).context("invalid connection request JSON")?;
    let server = if request.server.trim().is_empty() {
        load_preferences(&state.preferences_path)
            .unwrap_or_default()
            .server
    } else {
        request.server
    };
    let connection = probe_server_connection(&server);
    Ok(response_json(
        "200 OK",
        serde_json::json!({
            "status": connection.status,
            "message": connection.message,
            "version": connection.version,
            "latency_ms": connection.latency_ms
        }),
    ))
}
