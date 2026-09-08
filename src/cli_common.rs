//! Small, versioned CLI contract. Product state machines remain product-owned.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct Failure {
    pub exit: u8,
    pub code: &'static str,
    pub committed: bool,
    pub transaction_id: Option<String>,
}
pub type Result<T> = std::result::Result<T, Failure>;
pub fn fail(exit: u8, code: &'static str) -> Failure {
    Failure {
        exit,
        code,
        committed: false,
        transaction_id: None,
    }
}
pub fn input_error(_: impl std::fmt::Debug) -> Failure {
    fail(2, "invalid_input")
}
pub fn storage_error(_: impl std::fmt::Debug) -> Failure {
    fail(8, "unsafe_or_corrupt_state")
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for Failure {}

pub struct Args {
    pub words: Vec<String>,
    pub options: BTreeMap<String, String>,
    pub format: String,
    pub timeout: Duration,
}
impl Args {
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut words = vec![];
        let mut options = BTreeMap::new();
        let mut it = raw.into_iter();
        while let Some(arg) = it.next() {
            if arg.starts_with('-') {
                let name = match arg.as_str() {
                    "--output" => "--format",
                    "-h" => "--help",
                    "-V" => "--version",
                    _ => &arg,
                }
                .to_string();
                let value = match name.as_str() {
                    "--format"
                    | "--timeout"
                    | "--config"
                    | "--state"
                    | "--file"
                    | "--bootstrap"
                    | "--server"
                    | "--expected-revision"
                    | "--tail"
                    | "--since"
                    | "--expected-binding" => {
                        it.next().ok_or_else(|| fail(2, "missing_option_value"))?
                    }
                    "--interactive" | "--input-stdin" | "--non-interactive" | "--no-color"
                    | "--now" | "--watch" | "--check" | "--network" | "--sunshine"
                    | "--delivery" | "--follow" | "--confirm-replace" | "--help" | "--version" => {
                        "true".into()
                    }
                    "--json" => {
                        if options.insert("--format".into(), "json".into()).is_some() {
                            return Err(fail(2, "duplicate_option"));
                        }
                        continue;
                    }
                    _ => return Err(fail(2, "unknown_option")),
                };
                if options.insert(name, value).is_some() {
                    return Err(fail(2, "duplicate_option"));
                }
            } else {
                words.push(arg);
            }
        }
        if options.contains_key("--interactive")
            && (options.contains_key("--non-interactive") || options.contains_key("--input-stdin"))
        {
            return Err(fail(2, "conflicting_input_modes"));
        }
        let format = options.get("--format").cloned().unwrap_or("human".into());
        if !["human", "json", "ndjson"].contains(&format.as_str()) {
            return Err(fail(2, "invalid_format"));
        }
        let timeout = duration(
            options
                .get("--timeout")
                .map(String::as_str)
                .unwrap_or("60s"),
        )?;
        for name in ["--config", "--state", "--file", "--bootstrap"] {
            if let Some(value) = options.get(name) {
                absolute(Path::new(value))?;
            }
        }
        Ok(Self {
            words,
            options,
            format,
            timeout,
        })
    }
    pub fn has(&self, name: &str) -> bool {
        self.options.contains_key(name)
    }
    pub fn get(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }
    pub fn require(&self, name: &str) -> Result<&str> {
        self.get(name)
            .ok_or_else(|| fail(2, "missing_required_option"))
    }
    pub fn validate_options(&self, permitted: &[&str]) -> Result<()> {
        for name in self.options.keys() {
            if ![
                "--format",
                "--timeout",
                "--config",
                "--state",
                "--non-interactive",
                "--no-color",
                "--help",
                "--version",
            ]
            .contains(&name.as_str())
                && !permitted.contains(&name.as_str())
            {
                return Err(fail(2, "option_not_valid_for_command"));
            }
        }
        Ok(())
    }
}
pub fn duration(raw: &str) -> Result<Duration> {
    let (n, m) = if let Some(s) = raw.strip_suffix("ms") {
        (s, 1)
    } else if let Some(s) = raw.strip_suffix('s') {
        (s, 1000)
    } else if let Some(s) = raw.strip_suffix('m') {
        (s, 60_000)
    } else {
        (raw, 1000)
    };
    let ms = n
        .parse::<u64>()
        .map_err(input_error)?
        .checked_mul(m)
        .ok_or_else(|| fail(2, "invalid_timeout"))?;
    if ms == 0 || ms > 3_600_000 {
        return Err(fail(2, "invalid_timeout"));
    }
    Ok(Duration::from_millis(ms))
}
pub fn absolute(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(fail(2, "absolute_path_required"));
    }
    Ok(())
}
pub fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn sanitize(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}
pub fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if ["password", "token", "credential", "secret", "enrollment"]
                    .iter()
                    .any(|k| key.contains(k))
                    && (v.is_string() || v.is_null())
                {
                    *v = json!({"configured":!v.is_null() && v.as_str().is_none_or(|s|!s.is_empty())});
                } else {
                    redact(v);
                }
            }
        }
        Value::Array(a) => {
            for v in a {
                redact(v)
            }
        }
        Value::String(s) => *s = sanitize(s),
        _ => (),
    }
}
pub fn emit(product: &str, command: &str, format: &str, result: &Result<Value>) -> u8 {
    let (exit, mut value) = match result {
        Ok(v) => (
            0,
            json!({"schema_version":1,"product":product,"command":command,"ok":true,"result":v}),
        ),
        Err(e) => (
            e.exit,
            json!({"schema_version":1,"product":product,"command":command,"ok":false,"error":{"code":e.code,"retryable":matches!(e.exit,5|6|9),"committed":e.committed,"transaction_id":e.transaction_id,"next_step":if e.exit==9 {"inspect pair status; resume the same transaction"} else if e.exit==5 {"stop the service and retry"} else {"inspect configuration and local diagnostics"}}}),
        ),
    };
    if let Ok(Value::Object(fields)) = result {
        for (key, field) in fields {
            if !["schema_version", "product", "command", "ok", "result"].contains(&key.as_str()) {
                value[key] = field.clone();
            }
        }
    }
    redact(&mut value);
    // Every finite command emits exactly one object. Never interpolate untrusted text.
    let encoded = if format == "human" {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    };
    if writeln!(io::stdout().lock(), "{}", encoded.unwrap_or_default()).is_err() {
        return if exit == 0 { 11 } else { exit };
    }
    exit
}
pub fn stdin_document<T: serde::de::DeserializeOwned + Send + 'static>(
    timeout: Duration,
) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = (|| {
            let mut bytes = Vec::new();
            io::stdin()
                .lock()
                .take(65537)
                .read_to_end(&mut bytes)
                .map_err(input_error)?;
            if bytes.len() > 65536 {
                bytes.fill(0);
                return Err(fail(2, "input_too_large"));
            }
            let result = serde_json::from_slice(&bytes).map_err(input_error);
            bytes.fill(0);
            result
        })();
        let _ = tx.send(result);
    });
    rx.recv_timeout(timeout)
        .map_err(|_| fail(9, "protected_input_timeout"))?
}
pub fn prompt(label: &str, secret: bool) -> Result<String> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(fail(2, "interactive_terminal_required"));
    }
    eprint!("{label}: ");
    io::stderr().flush().map_err(input_error)?;
    #[cfg(unix)]
    {
        struct EchoGuard(Option<String>);
        impl Drop for EchoGuard {
            fn drop(&mut self) {
                if let Some(mode) = &self.0 {
                    let _ = Command::new("/bin/stty")
                        .arg(mode)
                        .stdin(Stdio::inherit())
                        .status();
                    eprintln!();
                }
            }
        }
        let mode = if secret {
            let o = Command::new("/bin/stty")
                .arg("-g")
                .stdin(Stdio::inherit())
                .output()
                .map_err(input_error)?;
            if !o.status.success() {
                return Err(fail(2, "terminal_unavailable"));
            }
            if !Command::new("/bin/stty")
                .arg("-echo")
                .stdin(Stdio::inherit())
                .status()
                .map_err(input_error)?
                .success()
            {
                return Err(fail(2, "terminal_unavailable"));
            }
            Some(
                String::from_utf8(o.stdout)
                    .map_err(input_error)?
                    .trim()
                    .into(),
            )
        } else {
            None
        };
        let _guard = EchoGuard(mode);
        let mut value = String::new();
        io::stdin().read_line(&mut value).map_err(input_error)?;
        if value.len() > 65536 {
            return Err(fail(2, "input_too_large"));
        }
        Ok(value.trim_end_matches(['\r', '\n']).into())
    }
    #[cfg(windows)]
    {
        if secret {
            let o=Command::new("powershell.exe").args(["-NoProfile","-Command","$s=Read-Host -AsSecureString; $p=[Runtime.InteropServices.Marshal]::SecureStringToBSTR($s); try {[Console]::Write([Runtime.InteropServices.Marshal]::PtrToStringBSTR($p))} finally {[Runtime.InteropServices.Marshal]::ZeroFreeBSTR($p)}"]).stdin(Stdio::inherit()).output().map_err(input_error)?;
            if !o.status.success() {
                return Err(fail(2, "terminal_unavailable"));
            }
            String::from_utf8(o.stdout).map_err(input_error)
        } else {
            let mut s = String::new();
            io::stdin().read_line(&mut s).map_err(input_error)?;
            Ok(s.trim_end_matches(['\r', '\n']).into())
        }
    }
}

pub struct Service {
    pub name: &'static str,
    #[allow(dead_code)]
    pub label: &'static str,
    pub default_config: PathBuf,
    pub binary: &'static str,
}
impl Service {
    fn capture(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<std::process::Output> {
        // Commands have fixed executables/verbs. No shell, no user-supplied service names.
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| fail(6, "service_manager_unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| fail(8, "output_pipe_unavailable"))?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let deadline = Instant::now() + timeout;
        loop {
            if child.try_wait().map_err(storage_error)?.is_some() {
                let status = child.wait().map_err(storage_error)?;
                let bytes = reader
                    .join()
                    .map_err(|_| fail(8, "output_reader_unavailable"))?
                    .map_err(storage_error)?;
                if bytes.len() > 4 * 1024 * 1024 {
                    return Err(fail(8, "output_budget_exceeded"));
                }
                return Ok(std::process::Output {
                    status,
                    stdout: bytes,
                    stderr: Vec::new(),
                });
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(fail(9, "service_timeout"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    pub fn status(&self, timeout: Duration) -> Result<Value> {
        #[cfg(target_os = "linux")]
        {
            let o = self.capture(
                "/usr/bin/systemctl",
                &[
                    "show",
                    self.name,
                    "--property=LoadState,ActiveState,UnitFileState,ExecStart",
                ],
                timeout,
            )?;
            if !o.status.success() {
                return Err(fail(6, "service_manager_unavailable"));
            }
            let text = String::from_utf8_lossy(&o.stdout);
            let fields: BTreeMap<_, _> = text.lines().filter_map(|l| l.split_once('=')).collect();
            let active = fields.get("ActiveState").copied().unwrap_or("unknown");
            Ok(
                json!({"installed":fields.get("LoadState")==Some(&"loaded"),"state":if active=="active" {"running"} else if active=="inactive" || active=="failed" {"stopped"} else {active},"startup":fields.get("UnitFileState"),"registration":fields.get("ExecStart")}),
            )
        }
        #[cfg(windows)]
        {
            let o = self.capture("sc.exe", &["query", self.name], timeout)?;
            let t = String::from_utf8_lossy(&o.stdout);
            let config = self.capture("sc.exe", &["qc", self.name], timeout)?;
            let config = String::from_utf8_lossy(&config.stdout);
            let registration = config.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("BINARY_PATH_NAME")
                    .and_then(|v| v.trim().strip_prefix(':'))
                    .map(str::trim)
            });
            Ok(
                json!({"installed":o.status.success(),"state":if t.contains("RUNNING"){"running"}else if t.contains("STOPPED"){"stopped"}else{"unknown"},"startup":if config.contains("AUTO_START"){"automatic"}else if config.contains("DEMAND_START"){"manual"}else if config.contains("DISABLED"){"disabled"}else{"unknown"},"registration":registration}),
            )
        }
        #[cfg(target_os = "macos")]
        {
            let target = format!("system/{}", self.label);
            let o = self.capture("/bin/launchctl", &["print", &target], timeout)?;
            let t = String::from_utf8_lossy(&o.stdout);
            let plist = format!("/Library/LaunchDaemons/{}.plist", self.label);
            let policy = self.capture("/bin/launchctl", &["print-disabled", "system"], timeout)?;
            if !policy.status.success() {
                return Err(fail(6, "service_manager_unavailable"));
            }
            let policy = String::from_utf8_lossy(&policy.stdout);
            let disabled = policy.lines().any(|line| {
                line.contains(&format!("\"{}\"", self.label))
                    && line
                        .split_once("=>")
                        .is_some_and(|(_, value)| value.trim() == "true")
            });
            Ok(json!({
                "installed": Path::new(&plist).is_file(),
                "loaded": o.status.success(),
                "state": if o.status.success() && t.contains("state = running") {"running"} else {"stopped"},
                "startup": if disabled {"disabled"} else {"automatic"}
            }))
        }
    }
    pub fn change(&self, args: &Args, action: &str, selected: &Path) -> Result<Value> {
        if selected != self.default_config {
            return Err(fail(2, "service_config_mismatch"));
        }
        let before = self.status(args.timeout)?;
        if before["installed"] != true {
            return Err(fail(4, "service_not_installed"));
        }
        #[cfg(any(target_os = "linux", windows))]
        {
            let registration = before["registration"].as_str().unwrap_or("");
            if !registration.contains(self.binary)
                || !registration.contains(self.default_config.to_string_lossy().as_ref())
            {
                return Err(fail(8, "service_registration_mismatch"));
            }
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::MetadataExt;
            let plist = format!("/Library/LaunchDaemons/{}.plist", self.label);
            let meta = std::fs::symlink_metadata(&plist)
                .map_err(|_| fail(8, "unsafe_service_registration"))?;
            if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 || meta.nlink() != 1 {
                return Err(fail(8, "unsafe_service_registration"));
            }
            let output = self.capture(
                "/usr/bin/plutil",
                &["-extract", "ProgramArguments", "json", "-o", "-", &plist],
                args.timeout,
            )?;
            let registered: Vec<String> = serde_json::from_slice(&output.stdout)
                .map_err(|_| fail(8, "service_registration_mismatch"))?;
            let expected_binary = format!("/usr/local/libexec/{}", self.binary);
            if !output.status.success()
                || registered.first() != Some(&expected_binary)
                || registered.len() != 4
                || registered[1] != "run"
                || !["--config", "--state"].contains(&registered[2].as_str())
                || registered[3] != self.default_config.to_string_lossy()
            {
                return Err(fail(8, "service_registration_mismatch"));
            }
        }
        if !["start", "stop", "restart", "enable", "disable"].contains(&action) {
            return Err(fail(2, "invalid_service_action"));
        }
        #[cfg(target_os = "linux")]
        let output = {
            let mut cmd = vec![action, self.name];
            if args.has("--now") {
                cmd.push("--now");
            }
            self.capture("/usr/bin/systemctl", &cmd, args.timeout)?
        };
        #[cfg(windows)]
        let output = {
            if action == "start" && before["state"] == "running"
                || action == "stop" && before["state"] == "stopped"
            {
                return Ok(before);
            }
            if action == "restart" && before["state"] != "stopped" {
                let o = self.capture("sc.exe", &["stop", self.name], args.timeout)?;
                if !o.status.success() {
                    return Err(fail(3, "service_action_denied"));
                }
                self.wait("stopped", args.timeout)?;
            }
            let o = match action {
                "enable" => self.capture(
                    "sc.exe",
                    &["config", self.name, "start=", "auto"],
                    args.timeout,
                )?,
                "disable" => self.capture(
                    "sc.exe",
                    &["config", self.name, "start=", "demand"],
                    args.timeout,
                )?,
                "restart" => self.capture("sc.exe", &["start", self.name], args.timeout)?,
                _ => self.capture("sc.exe", &[action, self.name], args.timeout)?,
            };
            if o.status.success()
                && args.has("--now")
                && ["enable", "disable"].contains(&action)
                && !(action == "enable" && before["state"] == "running"
                    || action == "disable" && before["state"] == "stopped")
            {
                self.capture(
                    "sc.exe",
                    &[if action == "enable" { "start" } else { "stop" }, self.name],
                    args.timeout,
                )?
            } else {
                o
            }
        };
        #[cfg(target_os = "macos")]
        let output = {
            let target = format!("system/{}", self.label);
            let plist = format!("/Library/LaunchDaemons/{}.plist", self.label);
            let loaded = before["loaded"] == true;
            let should_start =
                ["start", "restart"].contains(&action) || action == "enable" && args.has("--now");
            let should_stop = action == "stop" || action == "disable" && args.has("--now");
            let policy_change = ["enable", "disable"].contains(&action);
            let policy_output = if policy_change {
                let o = self.capture("/bin/launchctl", &[action, &target], args.timeout)?;
                if !o.status.success() {
                    return Err(fail(3, "service_action_denied"));
                }
                Some(o)
            } else {
                None
            };
            if should_start {
                // launchd refuses bootstrap for disabled jobs. Restore startup policy
                // after the explicit one-off start, including when bootstrap fails.
                let restore_disabled = before["startup"] == "disabled" && !policy_change;
                if restore_disabled {
                    let o = self.capture("/bin/launchctl", &["enable", &target], args.timeout)?;
                    if !o.status.success() {
                        return Err(fail(3, "service_action_denied"));
                    }
                }
                let result = if loaded {
                    let cmd = if action == "restart" {
                        vec!["kickstart", "-k", &target]
                    } else {
                        vec!["kickstart", &target]
                    };
                    self.capture("/bin/launchctl", &cmd, args.timeout)
                } else {
                    self.capture(
                        "/bin/launchctl",
                        &["bootstrap", "system", &plist],
                        args.timeout,
                    )
                };
                if restore_disabled {
                    let restored = self
                        .capture("/bin/launchctl", &["disable", &target], args.timeout)
                        .map_err(|_| fail(11, "startup_policy_restore_failed"))?;
                    if !restored.status.success() {
                        return Err(fail(11, "startup_policy_restore_failed"));
                    }
                }
                result?
            } else if should_stop && loaded {
                self.capture("/bin/launchctl", &["bootout", &target], args.timeout)?
            } else if let Some(output) = policy_output {
                output
            } else {
                return Ok(before);
            }
        };
        if !output.status.success() {
            return Err(fail(3, "service_action_denied"));
        }
        if ["start", "restart", "stop"].contains(&action) || args.has("--now") {
            self.wait(
                if ["stop", "disable"].contains(&action) {
                    "stopped"
                } else {
                    "running"
                },
                args.timeout,
            )?;
        }
        self.status(args.timeout)
    }
    fn wait(&self, expected: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.status(timeout)?["state"] == expected {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(fail(9, "service_state_unconfirmed"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Service {
    pub fn logs(&self, args: &Args) -> Result<Value> {
        let tail = args
            .get("--tail")
            .unwrap_or("100")
            .parse::<usize>()
            .map_err(input_error)?;
        if tail == 0 || tail > 1000 {
            return Err(fail(2, "invalid_log_tail"));
        }
        #[cfg(target_os = "linux")]
        {
            let tail = tail.to_string();
            let mut query = vec![
                "--no-pager",
                "--output=json",
                "--unit",
                self.name,
                "--lines",
                &tail,
            ];
            if let Some(cursor) = args.get("--log-cursor") {
                query.extend(["--after-cursor", cursor]);
            } else if let Some(since) = args.get("--since") {
                if since.len() > 64 || since.chars().any(char::is_control) {
                    return Err(fail(2, "invalid_log_since"));
                }
                query.extend(["--since", since]);
            }
            let output = self.capture("/usr/bin/journalctl", &query, args.timeout)?;
            if !output.status.success() {
                return Err(fail(3, "logs_unavailable"));
            }
            if output.stdout.len() > 4 * 1024 * 1024 {
                return Err(fail(8, "log_budget_exceeded"));
            }
            let mut entries = Vec::new();
            for line in output
                .stdout
                .split(|b| *b == b'\n')
                .filter(|l| !l.is_empty())
            {
                let value: Value = serde_json::from_slice(line).map_err(storage_error)?;
                let message = value["MESSAGE"].as_str().unwrap_or("");
                // Legacy text logs may predate CLI redaction. Suppress secret-bearing lines.
                let lower = message.to_ascii_lowercase();
                let message = if [
                    "password",
                    "token",
                    "secret",
                    "bearer",
                    "authorization",
                    "activation",
                    "enrollment",
                ]
                .iter()
                .any(|s| lower.contains(s))
                {
                    "[sensitive log message redacted]".into()
                } else {
                    sanitize(message)
                };
                entries.push(json!({"observed_at":value["__REALTIME_TIMESTAMP"],"cursor":value["__CURSOR"],"message":message}));
            }
            Ok(json!({"entries":entries,"source":"journald"}))
        }
        #[cfg(windows)]
        {
            let since = args.get("--since").unwrap_or("1970-01-01T00:00:00Z");
            if since.len() > 40
                || !since
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b"-:TZ+. ".contains(&b))
            {
                return Err(fail(2, "invalid_log_since"));
            }
            let script = format!(
                "$ErrorActionPreference='Stop'; $name=(Get-Service -Name '{}').DisplayName; $since=[DateTimeOffset]::Parse('{}').UtcDateTime; $items=@(Get-WinEvent -FilterHashtable @{{LogName='System';ProviderName='Service Control Manager';StartTime=$since}} -MaxEvents 1000 -ErrorAction SilentlyContinue | Where-Object {{$_.Properties.Count -gt 0 -and $_.Properties[0].Value -eq $name}} | Select-Object -First {} | ForEach-Object {{@{{observed_at=$_.TimeCreated.ToUniversalTime().ToString('o');cursor=[string]$_.RecordId;message=$_.Message}}}}); ConvertTo-Json -InputObject $items -Compress",
                self.name, since, tail
            );
            let output = self.capture(
                "powershell.exe",
                &["-NoProfile", "-NonInteractive", "-Command", &script],
                args.timeout,
            )?;
            if !output.status.success() {
                return Err(fail(3, "logs_unavailable"));
            }
            let mut entries: Vec<Value> =
                serde_json::from_slice(&output.stdout).map_err(storage_error)?;
            let cursor = args
                .get("--log-cursor")
                .and_then(|c| c.parse::<u64>().ok())
                .unwrap_or(0);
            entries.retain(|e| {
                e["cursor"]
                    .as_str()
                    .and_then(|c| c.parse::<u64>().ok())
                    .is_some_and(|c| c > cursor)
            });
            entries.reverse();
            for entry in &mut entries {
                entry["message"] = json!(log_message(entry["message"].as_str().unwrap_or("")));
            }
            Ok(json!({"source":"scm_lifecycle","entries":entries}))
        }
        #[cfg(target_os = "macos")]
        {
            use std::{
                io::{Read, Seek, SeekFrom},
                os::unix::fs::MetadataExt,
            };
            let path = if self.binary == "host-monitor" {
                "/var/log/host-monitor.log"
            } else {
                "/var/log/sunshine-client.log"
            };
            let metadata = std::fs::symlink_metadata(path).map_err(storage_error)?;
            if !metadata.is_file() || metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
                return Err(fail(8, "unsafe_log_file"));
            }
            let mut file = std::fs::File::open(path).map_err(storage_error)?;
            let held = file.metadata().map_err(storage_error)?;
            if held.ino() != metadata.ino() || held.dev() != metadata.dev() {
                return Err(fail(8, "log_changed_during_open"));
            }
            let length = held.len();
            let inode = held.ino();
            let resume = args
                .get("--log-cursor")
                .and_then(|c| c.split_once(':'))
                .and_then(|(i, o)| Some((i.parse::<u64>().ok()?, o.parse::<u64>().ok()?)))
                .filter(|(i, o)| *i == inode && *o <= length)
                .map(|(_, o)| o);
            let start = resume.unwrap_or(length.saturating_sub(1024 * 1024));
            file.seek(SeekFrom::Start(start)).map_err(storage_error)?;
            let mut bytes = Vec::new();
            file.take(1024 * 1024)
                .read_to_end(&mut bytes)
                .map_err(storage_error)?;
            let end = bytes
                .iter()
                .rposition(|b| *b == b'\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            let cursor = format!("{inode}:{}", start + end as u64);
            let text = String::from_utf8_lossy(&bytes[..end]);
            let mut lines: Vec<_> = text.lines().collect();
            if start > 0 && resume.is_none() && !lines.is_empty() {
                lines.remove(0);
            }
            if let Some(since) = args.get("--since") {
                if since.len() > 40
                    || !since
                        .bytes()
                        .all(|b| b.is_ascii_digit() || b"-:TZ. ".contains(&b))
                {
                    return Err(fail(2, "invalid_log_since"));
                }
                lines.retain(|line| {
                    line.chars().next().is_some_and(|c| c.is_ascii_digit()) && *line >= since
                });
            }
            let skip = lines.len().saturating_sub(tail);
            let entries: Vec<_> = lines
                .into_iter()
                .skip(skip)
                .map(|line| json!({"message":log_message(line),"cursor":cursor}))
                .collect();
            Ok(json!({"source":"service_log","entries":entries}))
        }
    }
}

pub fn follow_logs(product: &str, service: &Service, mut args: Args) -> u8 {
    if let Err(error) = args.validate_options(&["--tail", "--since", "--follow"]) {
        return emit(product, "logs", "ndjson", &Err(error));
    }
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return 8,
    };
    rt.block_on(async {let deadline=tokio::time::Instant::now()+args.timeout;loop {
        let result=service.logs(&args);
        match result {
            Err(e)=>return emit(product,"logs","ndjson",&Err(e)),
            Ok(value)=>if let Some(entries)=value["entries"].as_array(){for entry in entries {
                if let Some(cursor)=entry["cursor"].as_str(){args.options.insert("--log-cursor".into(),cursor.into());}
                let code=emit(product,"logs","ndjson",&Ok(entry.clone()));if code!=0{return code;}
            }},
        }
        tokio::select!{_=tokio::signal::ctrl_c()=>return 130,_=tokio::time::sleep_until(deadline)=>return 0,_=tokio::time::sleep(Duration::from_secs(1))=>{}}
    }})
}

#[cfg(not(target_os = "linux"))]
fn log_message(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if [
        "password",
        "token",
        "secret",
        "bearer",
        "authorization",
        "activation",
        "enrollment",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        "[sensitive log message redacted]".into()
    } else {
        sanitize(message)
    }
}
