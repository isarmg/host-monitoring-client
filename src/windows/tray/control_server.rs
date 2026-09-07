pub fn entry() -> anyhow::Result<()> {
    let arguments = std::env::args_os()
        .skip(1)
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| anyhow::anyhow!("tray arguments must be valid Unicode"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    match parse_tray_arguments(arguments)? {
        TrayCommand::Tray { open } => run_tray(open),
        TrayCommand::ElevatedPair {
            server,
            callback_nonce,
        } => elevated_pair(server, callback_nonce),
        TrayCommand::ElevatedService { action, notify } => elevated_service(action, notify),
        TrayCommand::ElevatedStopForExit => elevated_stop_for_exit(),
    }
}

pub fn show_error(message: &str) {
    message_box(message, "host-monitor", MB_OK | MB_ICONERROR);
}

impl LocalControlServer {
    fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .context("failed to bind the private local configuration server")?;
        let port = listener.local_addr()?.port();
        let origin = format!("http://127.0.0.1:{port}");
        let preferences_path = local_preferences_path()?;
        let state = Arc::new(LocalControlState {
            bootstrap_tokens: Mutex::new(Vec::new()),
            sessions: Mutex::new(Vec::new()),
            operations: Mutex::new(Vec::new()),
            active_pairings: AtomicUsize::new(0),
            active_service_operations: AtomicUsize::new(0),
            preferences_path,
        });
        let thread_state = Arc::clone(&state);
        let thread_origin = origin.clone();
        thread::Builder::new()
            .name("host-monitoring-tray-http".into())
            .spawn(move || serve_local(listener, thread_origin, thread_state))
            .context("failed to start the private local configuration server")?;
        Ok(Self { origin, state })
    }

    fn open_configuration(&self) -> anyhow::Result<()> {
        self.open_configuration_at("status")
    }

    fn open_configuration_at(&self, section: &str) -> anyhow::Result<()> {
        ensure!(
            matches!(section, "status" | "pair"),
            "invalid local page section"
        );
        let token = random_secret();
        {
            let mut tokens = lock(&self.state.bootstrap_tokens);
            retain_valid_tokens(&mut tokens);
            ensure!(tokens.len() < 32, "too many pending local browser sessions");
            tokens.push(ExpiringToken {
                value: token.clone(),
                expires: Instant::now() + Duration::from_secs(5 * 60),
            });
        }
        if let Err(error) = open_browser(&format!("{}/#{token}:{section}", self.origin)) {
            let mut tokens = lock(&self.state.bootstrap_tokens);
            if let Some(index) = tokens.iter().position(|candidate| {
                constant_time_eq(candidate.value.as_bytes(), token.as_bytes())
            }) {
                tokens.remove(index);
            }
            return Err(error);
        }
        Ok(())
    }
}

fn serve_local(listener: TcpListener, origin: String, state: Arc<LocalControlState>) {
    let active = Arc::new(AtomicUsize::new(0));
    for connection in listener.incoming() {
        let Ok(mut stream) = connection else {
            continue;
        };
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_HTTP_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            let _ = write_response(
                &mut stream,
                response_json(
                    "503 Service Unavailable",
                    serde_json::json!({
                        "status": "error",
                        "code": "local_control_busy",
                        "message": "本地控制服务繁忙，请稍后重试"
                    }),
                ),
            );
            continue;
        }
        let state = Arc::clone(&state);
        let origin = origin.clone();
        let active = ActiveConnection(Arc::clone(&active));
        let _ = thread::Builder::new()
            .name("host-monitoring-tray-http-request".into())
            .spawn(move || {
                let _active = active;
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                let response = match read_request(&mut stream)
                    .and_then(|request| route_request(request, &origin, &state))
                {
                    Ok(response) => response,
                    Err(error) => local_api_error_response(&error),
                };
                let _ = write_response(&mut stream, response);
            });
    }
}

fn local_api_error_response(error: &anyhow::Error) -> HttpResponse {
    let message = error.to_string();
    let (status, code, public_message) =
        if message.contains("session") || message.contains("browser capability") {
            (
                "401 Unauthorized",
                "session_expired",
                "本地安全会话已失效，请从托盘菜单重新打开此页面".to_string(),
            )
        } else if message.contains("正在进行") || message.contains("too many") {
            (
                "409 Conflict",
                "operation_conflict",
                "另一个操作正在进行，请等待其完成后再试".to_string(),
            )
        } else if message.contains("service is not installed")
            || message.contains("Service Control Manager")
        {
            (
                "503 Service Unavailable",
                "service_unavailable",
                "Windows Client 服务不可用，请检查安装状态".to_string(),
            )
        } else {
            ("400 Bad Request", "invalid_request", message)
        };
    response_json(
        status,
        serde_json::json!({
            "status": "error",
            "code": code,
            "message": public_message
        }),
    )
}

fn read_request(stream: &mut TcpStream) -> anyhow::Result<LocalHttpRequest> {
    let mut received = Vec::new();
    let header_end = loop {
        if let Some(index) = find_bytes(&received, b"\r\n\r\n") {
            break index + 4;
        }
        ensure!(
            received.len() < MAX_LOCAL_HTTP_HEAD_BYTES,
            "request headers are too large"
        );
        let mut chunk = [0_u8; 2048];
        let read = stream.read(&mut chunk)?;
        ensure!(read != 0, "connection closed before request headers");
        received.extend_from_slice(&chunk[..read]);
    };
    ensure!(
        header_end <= MAX_LOCAL_HTTP_HEAD_BYTES,
        "request headers are too large"
    );
    let head = std::str::from_utf8(&received[..header_end - 4])
        .context("request headers are not valid UTF-8")?;
    ensure!(head.is_ascii(), "request headers must be ASCII");
    let mut lines = head.split("\r\n");
    let request_line = lines.next().context("missing request line")?;
    let parts = request_line.split_ascii_whitespace().collect::<Vec<_>>();
    ensure!(parts.len() == 3, "malformed request line");
    ensure!(parts[2] == "HTTP/1.1", "only HTTP/1.1 is supported");
    ensure!(
        matches!(parts[0], "GET" | "POST"),
        "unsupported HTTP method"
    );
    ensure!(
        parts[1].starts_with('/'),
        "request target must be origin-form"
    );
    ensure!(
        !parts[1].contains(['?', '#']),
        "query strings and fragments are not accepted"
    );
    let method = parts[0].to_string();
    let target = parts[1].to_string();
    let mut headers = BTreeMap::new();
    for line in lines {
        ensure!(
            !line.starts_with([' ', '\t']),
            "folded headers are not accepted"
        );
        let (name, value) = line.split_once(':').context("malformed request header")?;
        ensure!(
            !name.is_empty()
                && name.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(
                            byte,
                            b'!' | b'#'
                                ..=b'\''
                                    | b'*'
                                    | b'+'
                                    | b'-'
                                    | b'.'
                                    | b'^'
                                    | b'_'
                                    | b'`'
                                    | b'|'
                                    | b'~'
                        )
                }),
            "invalid request header name"
        );
        let name = name.to_ascii_lowercase();
        let value = value.trim().to_string();
        ensure!(
            headers.insert(name.clone(), value).is_none(),
            "duplicate request header: {name}"
        );
    }
    ensure!(
        !headers.contains_key("transfer-encoding"),
        "transfer encoding is not accepted"
    );
    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>().context("invalid Content-Length"))
        .transpose()?
        .unwrap_or(0);
    ensure!(
        content_length <= MAX_LOCAL_HTTP_BODY_BYTES,
        "request body is too large"
    );
    let expected = header_end + content_length;
    ensure!(
        received.len() <= expected,
        "HTTP pipelining is not accepted"
    );
    while received.len() < expected {
        let remaining = expected - received.len();
        let mut chunk = [0_u8; 2048];
        let read_length = remaining.min(chunk.len());
        let read = stream.read(&mut chunk[..read_length])?;
        ensure!(read != 0, "connection closed before request body");
        received.extend_from_slice(&chunk[..read]);
    }
    Ok(LocalHttpRequest {
        method,
        target,
        headers,
        body: received[header_end..].to_vec(),
    })
}
