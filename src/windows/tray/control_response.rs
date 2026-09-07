fn render_configuration() -> HttpResponse {
    response_html(
        "200 OK",
        "<main id=app aria-busy=true><header><div><h1>host-monitor</h1><p class=subtitle>Windows 本地控制与配对</p></div><span id=version class=version></span></header>\
             <div class=status-grid aria-label=Client状态>\
             <section class=status-card><h2>Windows 服务</h2><strong id=service aria-live=polite>正在建立安全的本地会话…</strong><p id=service-detail class=hint>服务状态来自 Windows SCM。</p></section>\
             <section class=status-card><h2>管理端可达性</h2><strong id=connection aria-live=polite>尚未检测</strong><button id=check-connection class=secondary type=button>立即检测</button><p class=hint>这里只检查管理端 /health/live，不代表遥测已成功上报。</p></section></div>\
             <section id=pair><h2>配对</h2><p class=hint>配对会替换此机器当前本地绑定，并在 Server 中创建新的主机实例；必要时会重启 Client 服务。</p>\
             <form data-endpoint=/pair><label for=server>服务器地址</label><input id=server name=server type=url maxlength=2048 required aria-describedby=server-hint placeholder=\"https://host-monitoring.example.com\">\
             <p id=server-hint class=hint>填写完整管理台 Origin（协议、主机和可选端口），不要附加路径。</p>\
             <label for=activation-code>一次性授权密钥</label><input id=activation-code name=activation_code type=password maxlength=256 required autocomplete=one-time-code spellcheck=false aria-describedby=code-hint placeholder=\"uci_…\">\
             <p id=code-hint class=hint>密钥只通过受保护的本地通道提交，不会保存到托盘偏好。</p>\
             <button id=pair-submit type=submit>开始配对</button></form></section>\
             <section><h2>服务控制</h2><p class=hint>停止只影响本次开机；服务启动类型保持 Automatic，下次开机仍会自动运行。</p><div class=actions>\
             <button data-service=start>启动服务</button><button data-service=stop class=danger>停止本次服务</button><button id=copy-diagnostics class=secondary type=button>复制脱敏诊断</button></div></section>\
             <div id=result class=result role=status aria-live=polite></div>\
             <footer>关闭此浏览器页面不会停止 Client。若要本次彻底退出，请使用托盘菜单“停止本次服务并退出托盘”。</footer></main><script src=/app.js defer></script>",
    )
}

fn require_origin(request: &LocalHttpRequest, origin: &str) -> anyhow::Result<()> {
    ensure!(
        request
            .headers
            .get("origin")
            .is_some_and(|value| constant_time_eq(value.as_bytes(), origin.as_bytes())),
        "Origin header does not match the loopback listener"
    );
    Ok(())
}

fn require_json_request(request: &LocalHttpRequest) -> anyhow::Result<()> {
    ensure!(
        request
            .headers
            .get("content-type")
            .is_some_and(|value| value.eq_ignore_ascii_case("application/json")),
        "Content-Type must be application/json"
    );
    ensure!(
        request.headers.contains_key("content-length"),
        "Content-Length is required"
    );
    Ok(())
}

fn require_control_marker(request: &LocalHttpRequest) -> anyhow::Result<()> {
    ensure!(
        request
            .headers
            .get("x-host-monitoring-tray")
            .is_some_and(|value| value == "1"),
        "missing local-control request marker"
    );
    Ok(())
}

fn bearer(request: &LocalHttpRequest) -> anyhow::Result<&str> {
    request
        .headers
        .get("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| value.len() == 64)
        .context("missing or invalid bearer capability")
}

fn response_html(status: &'static str, main: &str) -> HttpResponse {
    let body = format!(
        "<!doctype html><html lang=zh-CN><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>host-monitor</title><style>\
         :root{{font-family:Segoe UI,system-ui,sans-serif;color-scheme:light dark;line-height:1.45}}*{{box-sizing:border-box}}body{{margin:0;padding:24px;background:#f4f7fb;color:#172033}}main{{width:min(760px,100%);margin:3vh auto;padding:28px;background:#fff;border-radius:18px;box-shadow:0 12px 40px #16345b22}}header{{display:flex;justify-content:space-between;gap:20px;align-items:flex-start}}h1{{margin:0}}h2{{font-size:1.05rem;margin:0 0 12px}}.subtitle,.version,footer{{color:#58677c}}section{{border-top:1px solid #dce3ed;padding-top:20px;margin-top:24px}}.status-grid{{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:14px;margin-top:22px}}.status-card{{margin:0;padding:18px;border:1px solid #dce3ed;border-radius:12px}}label{{display:block;font-weight:600;margin:16px 0 6px}}input{{display:block;width:100%;padding:11px 12px;border:1px solid #aeb9c8;border-radius:8px;font:inherit}}button{{border:0;border-radius:8px;padding:10px 15px;background:#1769e0;color:#fff;font:inherit;font-weight:600;cursor:pointer}}button.secondary{{margin-top:12px;background:#e7eef9;color:#164b94}}button.danger{{background:#b42318}}button:disabled{{opacity:.55;cursor:not-allowed}}button:focus-visible,input:focus-visible{{outline:3px solid #75a7ff;outline-offset:2px}}.actions{{display:flex;flex-wrap:wrap;gap:10px}}.hint{{margin:7px 0;color:#58677c;font-size:.92rem}}.result{{min-height:3em;margin-top:22px;padding:12px 14px;border-radius:9px;background:#eef4ff}}.result[data-kind=error]{{background:#fde8e7;color:#8f1d16}}.result[data-kind=success]{{background:#e6f6ec;color:#176b39}}footer{{border-top:1px solid #dce3ed;margin-top:24px;padding-top:18px;font-size:.88rem}}@media(max-width:620px){{body{{padding:10px}}main{{padding:20px;margin:0 auto}}header{{display:block}}.status-grid{{grid-template-columns:1fr}}}}@media(prefers-color-scheme:dark){{body{{background:#10141d;color:#edf3ff}}main{{background:#1a2230}}section,footer,.status-card{{border-color:#39465a}}input{{background:#111824;color:#fff;border-color:#52627a}}.subtitle,.version,footer,.hint{{color:#b3bfd1}}button.secondary{{background:#2b3c57;color:#eaf2ff}}.result{{background:#202f47}}.result[data-kind=error]{{background:#4b2323;color:#ffd9d6}}.result[data-kind=success]{{background:#173c2a;color:#c9f7da}}}}@media(forced-colors:active){{button,input,.status-card,.result{{border:1px solid CanvasText}}button:focus-visible,input:focus-visible{{outline:2px solid Highlight}}}}</style></head><body>{main}</body></html>"
    );
    HttpResponse {
        status,
        content_type: "text/html; charset=utf-8",
        extra_headers: Vec::new(),
        body: body.into_bytes(),
    }
}

fn response_javascript(script: &'static str) -> HttpResponse {
    HttpResponse {
        status: "200 OK",
        content_type: "text/javascript; charset=utf-8",
        extra_headers: Vec::new(),
        body: script.as_bytes().to_vec(),
    }
}

fn response_json(status: &'static str, value: serde_json::Value) -> HttpResponse {
    HttpResponse {
        status,
        content_type: "application/json; charset=utf-8",
        extra_headers: Vec::new(),
        body: serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec()),
    }
}

fn write_response(stream: &mut TcpStream, response: HttpResponse) -> anyhow::Result<()> {
    let mut headers = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store, max-age=0\r\nPragma: no-cache\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    for (name, value) in response.extra_headers {
        ensure!(
            name.is_ascii() && value.is_ascii() && !value.contains(['\r', '\n']),
            "invalid local response header"
        );
        headers.push_str(&format!("{name}: {value}\r\n"));
    }
    headers.push_str("\r\n");
    stream.write_all(headers.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()?;
    Ok(())
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn retain_valid_tokens(tokens: &mut Vec<ExpiringToken>) {
    tokens.retain(|token| token.expires > Instant::now());
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}
