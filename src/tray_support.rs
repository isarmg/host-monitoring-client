//! Cross-platform helpers and health adapter for the Windows tray companion.
//!
//! The actual notification-area and elevation code is Windows-only, while
//! argument, URL, form and HTTP parsing stays here so it is exercised by the
//! normal cross-platform test suite.

use std::{collections::BTreeMap, net::IpAddr, time::Instant};

use anyhow::{Context, bail, ensure};

pub mod server_health;

pub const MAX_LOCAL_HTTP_HEAD_BYTES: usize = 16 * 1024;
pub const MAX_LOCAL_HTTP_BODY_BYTES: usize = 16 * 1024;

/// Converts an absolute I/O deadline into a finite Win32 wait interval.
/// Positive sub-millisecond durations round up so progress cannot accidentally
/// turn a live deadline into an immediate timeout. `INFINITE` is never returned.
pub fn deadline_wait_millis(now: Instant, deadline: Instant) -> Option<u32> {
    let remaining = deadline.checked_duration_since(now)?;
    if remaining.is_zero() {
        return None;
    }
    let rounded_millis = remaining
        .as_millis()
        .saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0));
    Some(rounded_millis.clamp(1, u128::from(u32::MAX - 1)) as u32)
}

/// Validates one partial Win32 pipe transfer and returns the bytes still due.
pub fn advance_pipe_transfer(remaining: usize, transferred: u32) -> anyhow::Result<usize> {
    ensure!(remaining != 0, "pipe transfer was already complete");
    let transferred = usize::try_from(transferred)?;
    ensure!(
        transferred != 0,
        "protected pairing pipe closed unexpectedly"
    );
    ensure!(
        transferred <= remaining,
        "invalid protected pairing pipe transfer count"
    );
    Ok(remaining - transferred)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayCommand {
    Tray {
        open: bool,
    },
    ElevatedPair {
        server: String,
        callback_nonce: String,
    },
    ElevatedService {
        action: ServiceAction,
        notify: bool,
    },
    ElevatedStopForExit,
}

/// Parses the tray's deliberately small, non-extensible privileged interface.
///
/// Keeping elevated modes fixed prevents the normal tray from becoming a
/// generic elevated process launcher.
pub fn parse_tray_arguments<I>(arguments: I) -> anyhow::Result<TrayCommand>
where
    I: IntoIterator<Item = String>,
{
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        // An explicit interactive launch (Start menu, post-install helper, or
        // double-click) should reveal the configuration surface. Login startup remains
        // silent through the distinct `--startup` argument.
        [] => Ok(TrayCommand::Tray { open: true }),
        [startup] if startup == "--startup" => Ok(TrayCommand::Tray { open: false }),
        [open] if open == "--open" => Ok(TrayCommand::Tray { open: true }),
        [mode, action]
            if matches!(
                mode.as_str(),
                "--elevated-service" | "--elevated-service-browser"
            ) =>
        {
            let action = match action.as_str() {
                "start" => ServiceAction::Start,
                "stop" => ServiceAction::Stop,
                _ => bail!("--elevated-service accepts only start or stop"),
            };
            Ok(TrayCommand::ElevatedService {
                action,
                notify: mode == "--elevated-service",
            })
        }
        [mode] if mode == "--elevated-stop-for-exit" => Ok(TrayCommand::ElevatedStopForExit),
        [mode, rest @ ..] if mode == "--elevated-pair" => parse_elevated_pair(rest),
        _ => bail!(
            "invalid tray arguments; expected --startup, --open, --elevated-pair, \
             --elevated-service, --elevated-service-browser, or --elevated-stop-for-exit"
        ),
    }
}

fn parse_elevated_pair(arguments: &[String]) -> anyhow::Result<TrayCommand> {
    let mut server = None;
    let mut callback_nonce = None;
    let mut index = 0;
    while index < arguments.len() {
        let flag = &arguments[index];
        index += 1;
        let value = arguments
            .get(index)
            .with_context(|| format!("{flag} requires a value"))?;
        index += 1;
        match flag.as_str() {
            "--server-b64" => {
                ensure!(server.is_none(), "--server-b64 may be specified only once");
                let value = decode_base64url(value).context("invalid encoded server URL")?;
                let value = String::from_utf8(value).context("server URL is not valid UTF-8")?;
                server = Some(validate_server_base(&value)?);
            }
            "--callback-nonce" => {
                ensure!(
                    callback_nonce.is_none(),
                    "--callback-nonce may be specified only once"
                );
                ensure!(
                    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "callback nonce must be exactly 256 bits of hexadecimal"
                );
                callback_nonce = Some(value.to_ascii_lowercase());
            }
            _ => bail!("unknown elevated pairing argument: {flag}"),
        }
    }
    let server = server.context("--elevated-pair requires --server-b64")?;
    let callback_nonce = callback_nonce.context("--elevated-pair requires --callback-nonce")?;
    Ok(TrayCommand::ElevatedPair {
        server,
        callback_nonce,
    })
}

/// Validates and normalizes the console base URL accepted by browser pairing.
/// Remote plaintext HTTP is intentionally not exposed through the tray UI.
pub fn validate_server_base(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    ensure!(!value.is_empty(), "server URL is required");
    ensure!(value.len() <= 2048, "server URL is too long");
    let url = sarmg_client_secure_http::Url::parse(value).context("invalid server URL")?;
    ensure!(!url.cannot_be_a_base(), "server URL must be hierarchical");
    ensure!(url.host_str().is_some(), "server URL must contain a host");
    ensure!(url.port() != Some(0), "server URL must not use port zero");
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "server URL must not embed credentials"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "server URL must not contain a query string or fragment"
    );
    ensure!(
        url.path() == "/",
        "server URL must be a complete management-console origin without a path; include only the scheme, host, and optional port"
    );
    sarmg_client_secure_http::client_network_policy(&url)
        .context("server violates Client network policy")?;
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// Validates a URL before handing it to ShellExecute. This is kept separate
/// from `validate_server_base` because activation URLs may contain a path,
/// query and fragment issued by the trusted Host Monitoring server.
pub fn validate_browser_url(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    ensure!(!value.is_empty(), "browser URL is empty");
    ensure!(value.len() <= 4096, "browser URL is too long");
    let url = sarmg_client_secure_http::Url::parse(value).context("invalid browser URL")?;
    ensure!(url.host_str().is_some(), "browser URL must contain a host");
    ensure!(url.port() != Some(0), "browser URL must not use port zero");
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "browser URL must not embed credentials"
    );
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(url.host_str()) => {}
        "http" => bail!("refusing to open a remote plaintext browser URL"),
        scheme => bail!("unsupported browser URL scheme: {scheme}"),
    }
    Ok(url.to_string())
}

pub fn browser_url_matches_server_origin(browser_url: &str, server: &str) -> bool {
    let Ok(browser_url) = sarmg_client_secure_http::Url::parse(browser_url) else {
        return false;
    };
    let Ok(server) = sarmg_client_secure_http::Url::parse(server) else {
        return false;
    };
    browser_url.scheme() == server.scheme()
        && browser_url.host_str().map(str::to_ascii_lowercase)
            == server.host_str().map(str::to_ascii_lowercase)
        && browser_url.port_or_known_default() == server.port_or_known_default()
}

/// Validate the one-time authorization key entered in the local tray page.
///
/// The value is intentionally returned by reference: callers can keep the
/// original allocation in memory and do not need to create another secret
/// copy merely for validation. The server applies the same upper bound and
/// whitespace rule at its anonymous activation endpoint.
pub fn validate_activation_code(value: &str) -> anyhow::Result<&str> {
    ensure!(!value.is_empty(), "authorization key is required");
    ensure!(
        value.len() <= 256,
        "authorization key must not exceed 256 bytes"
    );
    ensure!(
        !value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control()),
        "authorization key must not contain whitespace or control characters"
    );
    Ok(value)
}

pub(crate) fn is_loopback_host(host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

/// Parses a small application/x-www-form-urlencoded body and rejects duplicate
/// keys. Duplicate security-sensitive fields must never have ambiguous
/// first/last-value semantics.
pub fn parse_form(body: &[u8]) -> anyhow::Result<BTreeMap<String, String>> {
    ensure!(
        body.len() <= MAX_LOCAL_HTTP_BODY_BYTES,
        "local request body is too large"
    );
    let mut values = BTreeMap::new();
    if body.is_empty() {
        return Ok(values);
    }
    for field in body.split(|byte| *byte == b'&') {
        let mut parts = field.splitn(2, |byte| *byte == b'=');
        let key = decode_form_component(parts.next().unwrap_or_default())?;
        let value = decode_form_component(parts.next().unwrap_or_default())?;
        ensure!(!key.is_empty(), "form field name is empty");
        ensure!(
            values.insert(key.clone(), value).is_none(),
            "duplicate form field: {key}"
        );
    }
    Ok(values)
}

fn decode_form_component(value: &[u8]) -> anyhow::Result<String> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        match value[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' => {
                ensure!(
                    index + 2 < value.len(),
                    "truncated percent escape in form data"
                );
                decoded.push((decode_hex(value[index + 1])? << 4) | decode_hex(value[index + 2])?);
                index += 3;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    let decoded = String::from_utf8(decoded).context("form data is not valid UTF-8")?;
    ensure!(
        !decoded.contains('\0'),
        "form data must not contain NUL bytes"
    );
    Ok(decoded)
}

fn decode_hex(value: u8) -> anyhow::Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => bail!("invalid percent escape in form data"),
    }
}

pub fn html_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            character => escaped.push(character),
        }
    }
    escaped
}

/// Quotes one argument for ShellExecute's Windows command-line string using
/// the same backslash-before-quote rules as CommandLineToArgvW.
pub fn quote_windows_argument(value: &str) -> String {
    if !value.is_empty()
        && !value
            .chars()
            .any(|character| matches!(character, ' ' | '\t' | '\n' | '\u{000b}' | '"'))
    {
        return value.to_string();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for character in value.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            character => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                quoted.push(character);
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

pub fn encode_base64url(value: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity(value.len().div_ceil(3) * 4);
    for chunk in value.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        encoded.push(ALPHABET[(first >> 2) as usize] as char);
        encoded.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() >= 2 {
            encoded.push(ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        }
        if chunk.len() == 3 {
            encoded.push(ALPHABET[(third & 0x3f) as usize] as char);
        }
    }
    encoded
}

pub fn decode_base64url(value: &str) -> anyhow::Result<Vec<u8>> {
    ensure!(
        !value.contains('='),
        "base64url values must not contain padding"
    );
    ensure!(value.len() % 4 != 1, "invalid base64url length");
    let mut decoded = Vec::with_capacity(value.len() * 3 / 4);
    for chunk in value.as_bytes().chunks(4) {
        let mut values = [0_u8; 4];
        for (index, byte) in chunk.iter().enumerate() {
            values[index] = decode_base64url_character(*byte)?;
        }
        decoded.push((values[0] << 2) | (values[1] >> 4));
        if chunk.len() >= 3 {
            decoded.push((values[1] << 4) | (values[2] >> 2));
        }
        if chunk.len() == 4 {
            decoded.push((values[2] << 6) | values[3]);
        }
    }
    Ok(decoded)
}

fn decode_base64url_character(value: u8) -> anyhow::Result<u8> {
    match value {
        b'A'..=b'Z' => Ok(value - b'A'),
        b'a'..=b'z' => Ok(value - b'a' + 26),
        b'0'..=b'9' => Ok(value - b'0' + 52),
        b'-' => Ok(62),
        b'_' => Ok(63),
        _ => bail!("invalid base64url character"),
    }
}

pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn pipe_deadlines_are_finite_and_round_up() {
        let now = Instant::now();
        assert_eq!(deadline_wait_millis(now, now), None);
        assert_eq!(
            deadline_wait_millis(now, now - Duration::from_nanos(1)),
            None
        );
        assert_eq!(
            deadline_wait_millis(now, now + Duration::from_nanos(1)),
            Some(1)
        );
        assert_eq!(
            deadline_wait_millis(now, now + Duration::from_millis(1)),
            Some(1)
        );
        assert_eq!(
            deadline_wait_millis(
                now,
                now + Duration::from_millis(1) + Duration::from_nanos(1)
            ),
            Some(2)
        );
        assert_eq!(
            deadline_wait_millis(now, now + Duration::from_secs(u64::from(u32::MAX))),
            Some(u32::MAX - 1)
        );
    }

    #[test]
    fn partial_pipe_transfers_cannot_stall_or_overrun() {
        assert_eq!(advance_pipe_transfer(8, 3).unwrap(), 5);
        assert_eq!(advance_pipe_transfer(5, 5).unwrap(), 0);
        assert!(advance_pipe_transfer(5, 0).is_err());
        assert!(advance_pipe_transfer(5, 6).is_err());
        assert!(advance_pipe_transfer(0, 1).is_err());
    }

    #[test]
    fn privileged_argument_surface_is_strict() {
        assert_eq!(
            parse_tray_arguments(Vec::<String>::new()).unwrap(),
            TrayCommand::Tray { open: true }
        );
        assert_eq!(
            parse_tray_arguments(["--startup".into()]).unwrap(),
            TrayCommand::Tray { open: false }
        );
        assert_eq!(
            parse_tray_arguments(["--open".into()]).unwrap(),
            TrayCommand::Tray { open: true }
        );
        assert_eq!(
            parse_tray_arguments(["--elevated-service".into(), "start".into()]).unwrap(),
            TrayCommand::ElevatedService {
                action: ServiceAction::Start,
                notify: true,
            }
        );
        assert_eq!(
            parse_tray_arguments(["--elevated-service-browser".into(), "stop".into()]).unwrap(),
            TrayCommand::ElevatedService {
                action: ServiceAction::Stop,
                notify: false,
            }
        );
        assert_eq!(
            parse_tray_arguments(["--elevated-stop-for-exit".into()]).unwrap(),
            TrayCommand::ElevatedStopForExit
        );
        assert_eq!(
            parse_tray_arguments([
                "--elevated-pair".into(),
                "--server-b64".into(),
                encode_base64url(b"https://host-monitoring.example"),
                "--callback-nonce".into(),
                "ab".repeat(32),
            ])
            .unwrap(),
            TrayCommand::ElevatedPair {
                server: "https://host-monitoring.example".into(),
                callback_nonce: "ab".repeat(32),
            }
        );
        assert!(
            parse_tray_arguments(["--elevated-service".into(), "start".into(), "extra".into()])
                .is_err()
        );
        assert!(
            parse_tray_arguments([
                "--elevated-pair".into(),
                "--server-b64".into(),
                encode_base64url(b"https://host-monitoring.example"),
                "--server-b64".into(),
                encode_base64url(b"https://other.example"),
                "--callback-nonce".into(),
                "00".repeat(32)
            ])
            .is_err()
        );
    }

    #[test]
    fn server_url_rejects_dangerous_or_plaintext_remote_forms() {
        assert_eq!(
            validate_server_base(" https://host-monitoring.example/ ").unwrap(),
            "https://host-monitoring.example"
        );
        assert_eq!(
            validate_server_base("https://host-monitoring.example").unwrap(),
            "https://host-monitoring.example"
        );
        assert!(validate_server_base("http://127.0.0.1:18081").is_ok());
        assert!(validate_server_base("http://[::1]:18081").is_ok());
        assert!(validate_server_base("http://192.0.2.1").is_err());
        assert!(validate_server_base("file:///C:/Windows").is_err());
        assert!(validate_server_base("https://user:secret@example.test").is_err());
        assert!(validate_server_base("https://example.test/?token=secret").is_err());
        assert!(validate_server_base("https://example.test/console").is_err());
        assert!(validate_server_base("https://example.test/api/").is_err());
    }

    #[test]
    fn browser_url_allows_activation_paths_but_not_unsafe_schemes() {
        assert!(
            validate_browser_url("https://host-monitoring.example/activate/id?view=1#pair").is_ok()
        );
        assert!(validate_browser_url("http://localhost:3001/activate/id").is_ok());
        assert!(validate_browser_url("javascript:alert(1)").is_err());
        assert!(validate_browser_url("http://example.test/activate").is_err());
        assert!(browser_url_matches_server_origin(
            "https://host-monitoring.example/activate/id",
            "https://host-monitoring.example"
        ));
        assert!(browser_url_matches_server_origin(
            "https://host-monitoring.example:443/activate/id",
            "https://host-monitoring.example/console"
        ));
        assert!(!browser_url_matches_server_origin(
            "https://login.example/activate/id",
            "https://host-monitoring.example"
        ));
    }

    #[test]
    fn form_parser_decodes_utf8_and_rejects_ambiguity() {
        let form = parse_form(b"server=https%3A%2F%2Fhost-monitoring.example&name=Workstation+One")
            .unwrap();
        assert_eq!(form["server"], "https://host-monitoring.example");
        assert_eq!(form["name"], "Workstation One");
        assert!(parse_form(b"server=a&server=b").is_err());
        assert!(parse_form(b"name=%ZZ").is_err());
        assert!(parse_form(b"name=%FF").is_err());
    }

    #[test]
    fn activation_code_validation_matches_server_contract() {
        assert_eq!(
            validate_activation_code("uci_0123456789abcdef").unwrap(),
            "uci_0123456789abcdef"
        );
        assert!(validate_activation_code("").is_err());
        assert!(validate_activation_code("uci_contains space").is_err());
        assert!(validate_activation_code("uci_contains\0control").is_err());
        assert!(validate_activation_code(&"x".repeat(257)).is_err());
    }

    #[test]
    fn escaping_and_windows_quoting_cover_injection_characters() {
        assert_eq!(html_escape("<&\"'>"), "&lt;&amp;&quot;&#39;&gt;");
        assert_eq!(quote_windows_argument("simple"), "simple");
        assert_eq!(quote_windows_argument(""), "\"\"");
        assert_eq!(quote_windows_argument("two words"), "\"two words\"");
        assert_eq!(quote_windows_argument("ends\\"), "ends\\");
        assert_eq!(quote_windows_argument("a\\\"b"), "\"a\\\\\\\"b\"");
    }

    #[test]
    fn capability_comparison_does_not_accept_prefixes() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"samf"));
        assert!(!constant_time_eq(b"same", b"same-extra"));
    }

    #[test]
    fn base64url_round_trips_command_values_without_shell_metacharacters() {
        for value in [b"".as_slice(), b"f", b"fo", b"foo", "工作站 1".as_bytes()] {
            let encoded = encode_base64url(value);
            assert!(
                encoded
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            );
            assert_eq!(decode_base64url(&encoded).unwrap(), value);
        }
        assert!(decode_base64url("a").is_err());
        assert!(decode_base64url("abcd=").is_err());
    }
}
