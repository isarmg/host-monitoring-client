use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(not(unix))]
use crate::private_fs;
use anyhow::{Context, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use xcsc::secret::{SecretBytes, SecretString, SecretWriter};

// MSI persists this default before the first pairing. It must pass the same
// HTTPS-only policy as release binaries, even while no Manager is configured.
const DEFAULT_SERVER_ORIGIN: &str = "https://127.0.0.1:8081";
const PACKAGED_PLACEHOLDER_HOST: &str = "xsos.example.com";
const MAX_CONFIG_BYTES: usize = 64 * 1024;

const CLIENT_VERSION_OUTPUT: &str = concat!("xsoc ", env!("CARGO_PKG_VERSION"));

#[cfg(target_os = "linux")]
const fn nul_terminated<const N: usize>(value: &str) -> [u8; N] {
    let source = value.as_bytes();
    assert!(N == source.len() + 1);
    let mut output = [0; N];
    let mut index = 0;
    while index < source.len() {
        output[index] = source[index];
        index += 1;
    }
    output
}

/// Cross-built Linux packages cannot safely execute their target binary on the
/// packaging host. Keep an exact, NUL-terminated version record in a dedicated
/// ELF section so the package builder can inspect the payload without running it.
#[cfg(target_os = "linux")]
#[used]
// SAFETY: a unique product section contains only immutable, terminated version
// bytes; no executable instructions, pointers, or user data are linked here.
#[unsafe(link_section = ".xsoc.version")]
static LINUX_PACKAGE_VERSION_MARKER: [u8; CLIENT_VERSION_OUTPUT.len() + 1] =
    nul_terminated::<{ CLIENT_VERSION_OUTPUT.len() + 1 }>(CLIENT_VERSION_OUTPUT);

#[cfg(target_os = "linux")]
fn client_version_output() -> &'static str {
    // black_box keeps the output tied to the package marker under release LTO;
    // otherwise the optimizer could replace this read with another literal and
    // leave the custom section eligible for linker garbage collection.
    let marker = std::hint::black_box(&LINUX_PACKAGE_VERSION_MARKER);
    std::str::from_utf8(&marker[..marker.len() - 1])
        .expect("the compile-time Client version marker is valid UTF-8")
}

#[cfg(not(target_os = "linux"))]
fn client_version_output() -> &'static str {
    CLIENT_VERSION_OUTPUT
}

/// Server contract upper bound for the measured interval in reports.
///
/// `xsos-protocol` is the sole source of HTTP contract bounds, used by client configuration and server validation.
/// SQLite uses `0 < interval_seconds <= 3600` as a coarse storage constraint; client configuration uses
/// integer seconds (at least 1) and validates the worst jitter-adjusted cycle, so these bounds are related but distinct.
///
/// The report `interval_seconds` is actual elapsed time. Out-of-range values cause every report
/// to be permanently rejected with 400; the delivery worker acknowledges and discards these reports from spool.
/// Reject invalid configuration at startup rather than leaving users to trace recurring data gaps in logs.
pub const MAX_REPORT_INTERVAL_SECONDS: u64 = xsos_protocol::CLIENT_REPORT_MAX_INTERVAL_SECONDS;

/// Client configuration alias for the shared protocol lower bound.
///
/// Collection bounds the measured interval by this lower limit; configuration validation and report encoding use the shared protocol interval.
/// The server permanently rejects out-of-range reports, so sampling cannot rely solely on the current sleep duration.
pub const MIN_REPORT_INTERVAL_SECONDS: f64 = xsos_protocol::CLIENT_REPORT_MIN_INTERVAL_SECONDS;

/// Compile-time guard: the contract interval must be coherent. Use `const _` because this relationship
/// must hold for compilation to succeed rather than being discovered only when tests run.
const _: () = assert!(MIN_REPORT_INTERVAL_SECONDS > 0.0);
const _: () = assert!(MIN_REPORT_INTERVAL_SECONDS < MAX_REPORT_INTERVAL_SECONDS as f64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientCommand {
    Run,
    Once,
    Probe,
    Pair,
    Doctor,
    Status,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputMode {
    Json,
    #[default]
    Human,
}

#[derive(Clone, Copy)]
pub(crate) struct CurrentPackageVersion;

/// Persisted configuration discriminator. The wire field is `application_version`;
/// The current format is independent of binary patch versions.
pub const CONFIG_FORMAT_VERSION: &str = "1.0.0";

impl Serialize for CurrentPackageVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(CONFIG_FORMAT_VERSION)
    }
}

impl<'de> Deserialize<'de> for CurrentPackageVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = String::deserialize(deserializer)?;
        if version == CONFIG_FORMAT_VERSION {
            Ok(Self)
        } else {
            Err(D::Error::custom(format!(
                "unsupported configuration format {version}, expected {}",
                CONFIG_FORMAT_VERSION
            )))
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub(crate) application_version: CurrentPackageVersion,
    pub endpoint: String,
    /// Browser-authorized pairing endpoint. When omitted it is derived from
    /// the current report endpoint.
    pub pairing_endpoint: Option<String>,
    pub otlp_endpoint: Option<String>,
    #[serde(default, with = "crate::secret_io::optional")]
    pub otlp_token: Option<Arc<SecretString>>,
    pub interval_seconds: u64,
    pub slow_interval_seconds: u64,
    #[serde(default)]
    pub smart: crate::collectors::smart::SmartConfig,
    pub request_timeout_seconds: u64,
    pub jitter_percent: u8,
    pub state_dir: PathBuf,
    pub spool_max_bytes: u64,
    pub tls_identity_pem: Option<PathBuf>,
    pub tls_identity_pkcs12: Option<PathBuf>,
    #[serde(default, with = "crate::secret_io::optional")]
    pub tls_identity_password: Option<Arc<SecretString>>,
    pub tls_ca_pem: Option<PathBuf>,
    #[serde(skip)]
    pub config_path: Option<PathBuf>,
    #[serde(skip)]
    pub server_override: Option<String>,
    #[serde(skip)]
    pub endpoint_override: Option<String>,
    /// Explicit user-confirmed replacement of an incomplete saved request.
    /// Ordinary pairing remains resumable/fail-closed so a lost activation
    /// response cannot silently rotate secrets.
    #[serde(skip)]
    pub replace_pending_pairing: bool,
    /// Presentation is process-local and is never persisted.
    #[serde(skip)]
    pub output_mode: OutputMode,
    /// `doctor` is read-only by default. This explicit opt-in performs an
    /// end-to-end delivery probe for administrators who need it.
    #[serde(skip)]
    pub doctor_delivery: bool,
    /// A lenient `status` load records configuration trouble instead of
    /// preventing the one command intended to diagnose it.
    #[serde(skip)]
    pub config_issue: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            application_version: CurrentPackageVersion,
            endpoint: format!(
                "{DEFAULT_SERVER_ORIGIN}{}",
                xsos_protocol::CLIENT_REPORT_PATH
            ),
            pairing_endpoint: None,
            otlp_endpoint: None,
            otlp_token: None,
            interval_seconds: 10,
            slow_interval_seconds: 30,
            smart: Default::default(),
            request_timeout_seconds: 10,
            jitter_percent: 10,
            state_dir: default_state_dir(),
            spool_max_bytes: 64 * 1024 * 1024,
            tls_identity_pem: None,
            tls_identity_pkcs12: None,
            tls_identity_password: None,
            tls_ca_pem: None,
            config_path: None,
            server_override: None,
            endpoint_override: None,
            replace_pending_pairing: false,
            output_mode: OutputMode::Human,
            doctor_delivery: false,
            config_issue: None,
        }
    }
}

impl ClientConfig {
    pub fn load_from_args() -> anyhow::Result<(Self, ClientCommand)> {
        Self::load_from_iter(env::args().skip(1))
    }
    pub fn load_from_iter(
        arguments: impl IntoIterator<Item = String>,
    ) -> anyhow::Result<(Self, ClientCommand)> {
        let mut command = None;
        let mut windows_service = false;
        let mut config_path = env::var_os("XSOC_CONFIG").map(PathBuf::from);
        let mut server_override = None;
        let mut endpoint_override = None;
        let mut replace_pending_pairing = false;
        let mut output_mode = OutputMode::Human;
        let mut output_mode_selected = false;
        let mut doctor_delivery = false;
        let mut args = arguments.into_iter();
        let mut argument_position = 0usize;
        while let Some(arg) = args.next() {
            argument_position += 1;
            match arg.as_str() {
                "run" => select_command(&mut command, ClientCommand::Run, "run")?,
                "once" => select_command(&mut command, ClientCommand::Once, "once")?,
                "probe" => select_command(&mut command, ClientCommand::Probe, "probe")?,
                "pair" => select_command(&mut command, ClientCommand::Pair, "pair")?,
                "doctor" => select_command(&mut command, ClientCommand::Doctor, "doctor")?,
                "status" => select_command(&mut command, ClientCommand::Status, "status")?,
                crate::service::WINDOWS_SERVICE_ARGUMENT => {
                    validate_windows_service_position(argument_position)?;
                    if windows_service {
                        bail!("--windows-service may be specified only once");
                    }
                    windows_service = true;
                }
                "--config" => {
                    let value = args.next().context("--config requires a file path")?;
                    config_path = Some(PathBuf::from(value));
                }
                "--server" => {
                    server_override = Some(args.next().context("--server requires a URL")?);
                }
                "--endpoint" => {
                    endpoint_override =
                        Some(args.next().context("--endpoint requires a report URL")?);
                }
                "--replace-pending-pairing" => {
                    if replace_pending_pairing {
                        bail!("--replace-pending-pairing may be specified only once");
                    }
                    replace_pending_pairing = true;
                }
                "--output" => {
                    if output_mode_selected {
                        bail!("--output may be specified only once");
                    }
                    output_mode = match args
                        .next()
                        .context("--output requires human or json")?
                        .as_str()
                    {
                        "human" => OutputMode::Human,
                        "json" => OutputMode::Json,
                        other => bail!("unsupported output format {other}; use human or json"),
                    };
                    output_mode_selected = true;
                }
                "--json" => {
                    if output_mode_selected {
                        bail!("--json conflicts with an earlier output option");
                    }
                    output_mode = OutputMode::Json;
                    output_mode_selected = true;
                }
                "--delivery" => {
                    if doctor_delivery {
                        bail!("--delivery may be specified only once");
                    }
                    doctor_delivery = true;
                }
                "-V" | "--version" => {
                    println!("{}", client_version_output());
                    std::process::exit(0);
                }
                "-h" | "--help" => {
                    if windows_service {
                        bail!("--help is not available in Windows service mode");
                    }
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other}"),
            }
        }
        let command = command.unwrap_or(ClientCommand::Run);
        if doctor_delivery && command != ClientCommand::Doctor {
            bail!("--delivery may be used only with doctor");
        }
        validate_windows_service_invocation(windows_service, command)?;
        if replace_pending_pairing && command != ClientCommand::Pair {
            bail!("replacement requires pair");
        }

        if config_path.is_none() {
            let default = default_config_path();
            if default.is_file() {
                config_path = Some(default);
            }
        }
        let (mut config, config_issue) =
            Self::load_selected_config(config_path.as_deref(), command)?;
        config.apply_environment()?;
        config.config_path = config_path;
        config.server_override = server_override;
        config.endpoint_override = endpoint_override;
        config.replace_pending_pairing = replace_pending_pairing;
        config.output_mode = output_mode;
        config.doctor_delivery = doctor_delivery;
        config.config_issue = config_issue;
        if command == ClientCommand::Pair {
            config.apply_pair_options()?;
            config.validate_durable_report_endpoint(&config.endpoint)?;
        } else if let Some(endpoint) = config.endpoint_override.take() {
            config.endpoint = endpoint;
        }
        config.validate(command)?;
        Ok((config, command))
    }

    pub fn load_selected_config(
        config_path: Option<&Path>,
        command: ClientCommand,
    ) -> anyhow::Result<(Self, Option<String>)> {
        let Some(path) = config_path else {
            return Ok((Self::default(), None));
        };
        let loaded = fs::metadata(path)
            .and_then(|metadata| {
                if metadata.is_file() {
                    read_private_config(path).map_err(std::io::Error::other)
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "configuration path is not a regular file",
                    ))
                }
            })
            .and_then(|bytes| {
                serde_json::from_slice::<Self>(bytes.expose()).map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "invalid configuration at line {}, column {}",
                            error.line(),
                            error.column()
                        ),
                    )
                })
            });
        match loaded {
            Ok(config) => Ok((config, None)),
            Err(error) if command == ClientCommand::Status => Ok((
                Self::default(),
                Some(format!("failed to load {}: {error}", path.display())),
            )),
            Err(error) => {
                Err(error).with_context(|| format!("failed to load config {}", path.display()))
            }
        }
    }

    fn apply_pair_options(&mut self) -> anyhow::Result<()> {
        if self.server_override.is_some() && self.endpoint_override.is_some() {
            bail!("pair accepts either --server or --endpoint, not both");
        }
        if let Some(server) = self.server_override.as_deref() {
            let server = crate::pairing_input::validate_server_base(server)
                .context("invalid --server URL")?;
            self.endpoint = format!(
                "{}{}",
                server.trim_end_matches('/'),
                xsos_protocol::CLIENT_REPORT_PATH
            );
            self.pairing_endpoint = Some(format!(
                "{}{}",
                server.trim_end_matches('/'),
                xsos_protocol::CLIENT_PAIRING_REQUESTS_PATH
            ));
        } else if let Some(endpoint) = self.endpoint_override.take() {
            self.endpoint = endpoint;
            self.pairing_endpoint = None;
        }

        Ok(())
    }

    fn apply_environment(&mut self) -> anyhow::Result<()> {
        if let Ok(value) = env::var("XSOC_ENDPOINT") {
            self.endpoint = value;
        }
        if let Ok(value) = env::var("XSOC_PAIRING_ENDPOINT") {
            self.pairing_endpoint = non_empty(value);
        }
        if let Ok(value) = env::var("XSOC_OTLP_ENDPOINT") {
            self.otlp_endpoint = non_empty(value);
        }
        if let Ok(value) = env::var("XSOC_OTLP_TOKEN") {
            self.otlp_token = crate::secret_io::trimmed(value);
        }
        if let Ok(value) = env::var("XSOC_STATE_DIR") {
            self.state_dir = PathBuf::from(value);
        }
        if let Ok(value) = env::var("XSOC_INTERVAL_SECONDS") {
            self.interval_seconds = value.parse().context("invalid interval")?;
        }
        if let Ok(value) = env::var("XSOC_SLOW_INTERVAL_SECONDS") {
            self.slow_interval_seconds = value.parse().context("invalid slow interval")?;
        }
        if let Ok(value) = env::var("XSOC_TLS_IDENTITY_PEM") {
            self.tls_identity_pem = Some(PathBuf::from(value));
        }
        if let Ok(value) = env::var("XSOC_TLS_IDENTITY_PKCS12") {
            self.tls_identity_pkcs12 = Some(PathBuf::from(value));
        }
        if let Ok(value) = env::var("XSOC_TLS_IDENTITY_PASSWORD") {
            self.tls_identity_password = Some(Arc::new(SecretString::new(value)));
        }
        if let Ok(value) = env::var("XSOC_TLS_CA_PEM") {
            self.tls_ca_pem = Some(PathBuf::from(value));
        }
        Ok(())
    }

    pub fn validate(&self, command: ClientCommand) -> anyhow::Result<()> {
        // Status must be available precisely when configuration is missing or
        // malformed. It reports those conditions in its snapshot instead of
        // failing before any diagnostics can be rendered.
        if command == ClientCommand::Status {
            return Ok(());
        }
        if self.interval_seconds == 0 {
            bail!("interval_seconds must be greater than zero");
        }
        if self.jitter_percent > 50 {
            bail!("jitter_percent must not exceed 50");
        }
        // Ticker jitter determines the normal sampling cycle; a separate worker performs network delivery.
        // Validate the server reporting bound against maximum jitter: 3600 seconds with 10% jitter can reach
        // 3960 seconds. Reject excessive settings at startup and report the usable upper bound for the current jitter.
        let worst_case_cycle = self.worst_case_cycle_seconds();
        if worst_case_cycle > MAX_REPORT_INTERVAL_SECONDS as f64 {
            bail!(
                "interval_seconds ({}) with jitter_percent ({}) can produce a measured interval \
                 of up to {worst_case_cycle:.0}s, which exceeds the server contract limit of \
                 {MAX_REPORT_INTERVAL_SECONDS}s; such reports are rejected with HTTP 400 and \
                 discarded from the spool. Use interval_seconds <= {} at this jitter, \
                 or lower jitter_percent",
                self.interval_seconds,
                self.jitter_percent,
                self.max_interval_seconds_at_current_jitter()
            );
        }
        if !(60..=86400).contains(&self.smart.interval_seconds) {
            bail!("smart.interval_seconds must be between 60 and 86400");
        }
        if self
            .smart
            .executable
            .as_ref()
            .is_some_and(|p| !p.is_absolute())
        {
            bail!("smart.executable must be an absolute path");
        }
        if self.slow_interval_seconds < self.interval_seconds {
            bail!("slow_interval_seconds must be at least interval_seconds");
        }
        if self.request_timeout_seconds == 0 {
            bail!("request_timeout_seconds must be greater than zero");
        }
        if self.request_timeout_seconds > 300 {
            bail!("request_timeout_seconds must not exceed 300 seconds");
        }
        if self.spool_max_bytes < 1024 * 1024 {
            bail!("spool_max_bytes must be at least 1 MiB");
        }
        xcsc::runtime::SpoolLimits {
            max_record_bytes: crate::model::CLIENT_REPORT_MAX_BODY_BYTES,
            max_entries: xcsc::runtime::MAX_SPOOL_ENTRIES,
            max_bytes: self.spool_max_bytes,
        }
        .validate()
        .context("spool limits exceed the xcsc desktop-client profile")?;
        let validates_delivery = match command {
            ClientCommand::Probe => false,
            ClientCommand::Doctor => self.doctor_delivery,
            _ => true,
        };
        if validates_delivery {
            validate_endpoint(&self.endpoint)?;
            validate_pairing_endpoint(&self.pairing_endpoint())?;
        }
        #[cfg(not(feature = "otlp"))]
        if validates_delivery && (self.otlp_endpoint.is_some() || self.otlp_token.is_some()) {
            bail!(
                "OTLP export is configured but this Client was built without the optional `otlp` \
                 feature; rebuild with `--features otlp` or remove the OTLP settings"
            );
        }
        #[cfg(feature = "otlp")]
        if validates_delivery && let Some(endpoint) = &self.otlp_endpoint {
            validate_endpoint(endpoint)?;
        }
        if validates_delivery
            && self.tls_identity_pem.is_some()
            && self.tls_identity_pkcs12.is_some()
        {
            bail!("configure only one TLS client identity format");
        }
        if validates_delivery
            && self.tls_identity_password.is_some()
            && self.tls_identity_pkcs12.is_none()
        {
            bail!("tls_identity_password requires tls_identity_pkcs12");
        }
        #[cfg(all(not(windows), not(target_os = "macos")))]
        if validates_delivery && self.tls_identity_pkcs12.is_some() {
            bail!(
                "tls_identity_pkcs12 is supported only on Windows and macOS; use \
                 tls_identity_pem on this platform"
            );
        }
        #[cfg(any(windows, target_os = "macos"))]
        if validates_delivery && self.tls_identity_pem.is_some() {
            bail!(
                "the native TLS backend requires tls_identity_pkcs12 instead of \
                 tls_identity_pem"
            );
        }
        Ok(())
    }

    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_seconds)
    }

    pub fn diagnostic_config_issue(&self) -> Option<&str> {
        self.config_issue.as_deref()
    }

    /// Validate the effective configuration for a future `run` without
    /// requiring or changing credentials. Diagnostic renderers use this to
    /// report every problem instead of aborting before producing a result.
    pub fn validate_for_diagnostics(&self) -> anyhow::Result<()> {
        if let Some(issue) = &self.config_issue {
            bail!("{issue}");
        }
        self.validate(ClientCommand::Run)
    }

    /// Maximum duration in seconds to which jitter can extend a sampling cycle.
    ///
    /// Use the same upper-bound formula as `jitter()` in `main.rs`: `base * (1 + percent/100)`.
    /// Validation calls this function to prevent the two implementations from drifting.
    pub fn worst_case_cycle_seconds(&self) -> f64 {
        self.interval_seconds as f64 * (1.0 + self.jitter_percent as f64 / 100.0)
    }

    /// Maximum `interval_seconds` that satisfies the server contract with the current jitter.
    ///
    /// Round down so `worst_case_cycle_seconds()` for the resulting value never exceeds the contract upper bound.
    pub fn max_interval_seconds_at_current_jitter(&self) -> u64 {
        (MAX_REPORT_INTERVAL_SECONDS as f64 / (1.0 + self.jitter_percent as f64 / 100.0)) as u64
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_seconds)
    }

    pub fn pairing_endpoint(&self) -> String {
        self.pairing_endpoint.clone().unwrap_or_else(|| {
            if let Some(base) = self
                .endpoint
                .strip_suffix(xsos_protocol::CLIENT_REPORT_PATH)
            {
                return format!("{base}{}", xsos_protocol::CLIENT_PAIRING_REQUESTS_PATH);
            }
            let mut url = url::Url::parse(&self.endpoint)
                .expect("endpoint was validated before pairing_endpoint is used");
            url.set_path(xsos_protocol::CLIENT_PAIRING_REQUESTS_PATH);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string().trim_end_matches('/').to_string()
        })
    }

    /// The packaged example is a configuration sentinel, never a network target.
    pub fn uses_packaged_placeholder_server(&self) -> bool {
        [&self.endpoint, &self.pairing_endpoint()]
            .into_iter()
            .filter_map(|endpoint| url::Url::parse(endpoint).ok())
            .any(|endpoint| endpoint.host_str() == Some(PACKAGED_PLACEHOLDER_HOST))
    }

    pub(crate) fn validate_durable_report_endpoint(
        &self,
        report_endpoint: &str,
    ) -> anyhow::Result<()> {
        validate_endpoint(report_endpoint)
    }

    /// Save the browser-pairing endpoint only after the pending request has
    /// been durably recorded. This makes an interrupted `pair` resumable.
    pub fn persist_after_pairing(&self) -> anyhow::Result<PathBuf> {
        self.persist_durable_config()
    }

    pub fn persist_durable_config(&self) -> anyhow::Result<PathBuf> {
        let path = self.config_path.clone().unwrap_or_else(default_config_path);
        let mut persisted = self.clone();
        persisted.config_path = None;
        persisted.server_override = None;
        persisted.endpoint_override = None;
        persisted.replace_pending_pairing = false;
        let mut output = SecretWriter::new(MAX_CONFIG_BYTES)?;
        serde_json::to_writer_pretty(&mut output, &persisted)?;
        output.write_all(b"\n")?;
        let bytes = output.into_bytes();
        publish_private_config(&path, bytes.expose())?;
        Ok(path)
    }
}

fn select_command(
    selected: &mut Option<ClientCommand>,
    command: ClientCommand,
    spelling: &str,
) -> anyhow::Result<()> {
    if let Some(previous) = selected {
        bail!("multiple commands are not allowed (selected {previous:?}, then {spelling})");
    }
    *selected = Some(command);
    Ok(())
}

fn validate_windows_service_invocation(
    requested: bool,
    command: ClientCommand,
) -> anyhow::Result<()> {
    if !requested {
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = command;
        bail!("--windows-service is available only on Windows");
    }
    #[cfg(windows)]
    {
        if command != ClientCommand::Run {
            bail!("--windows-service may be used only with the run command");
        }
        Ok(())
    }
}

fn validate_windows_service_position(argument_position: usize) -> anyhow::Result<()> {
    if argument_position != 1 {
        bail!("--windows-service must be the first argument");
    }
    Ok(())
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

pub(crate) fn validate_endpoint(endpoint: &str) -> anyhow::Result<()> {
    let url = url::Url::parse(endpoint).context("invalid telemetry endpoint")?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.port_or_known_default().is_some(),
        "telemetry endpoint must use trusted HTTPS"
    );
    Ok(())
}

pub(crate) fn validate_pairing_endpoint(endpoint: &str) -> anyhow::Result<()> {
    validate_endpoint(endpoint).context("browser pairing requires HTTPS")?;
    let url = url::Url::parse(endpoint).expect("validate_endpoint accepted the URL");
    if url.query().is_some() || url.fragment().is_some() {
        bail!(
            "pairing_endpoint must not contain a query or fragment because request-specific paths \
             are appended while polling"
        );
    }
    Ok(())
}

fn default_state_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let base = env::var_os("PROGRAMDATA").unwrap_or_else(|| "C:\\ProgramData".into());
        PathBuf::from(base).join("xsoc")
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Library/Application Support/xsoc")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        PathBuf::from("/var/lib/xsoc")
    }
}

pub fn default_config_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        default_state_dir().join("config.json")
    }
    #[cfg(target_os = "macos")]
    {
        default_state_dir().join("config.json")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        PathBuf::from("/etc/xsoc/config.json")
    }
}

#[cfg(test)]
fn persist_private_config(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    path.parent().context("config path has no parent")?;
    if bytes.len() >= MAX_CONFIG_BYTES {
        bail!("config exceeds its {MAX_CONFIG_BYTES}-byte budget including the final newline");
    }
    let mut output = SecretWriter::new(MAX_CONFIG_BYTES)?;
    output.write_all(bytes)?;
    output.write_all(b"\n")?;
    publish_private_config(path, output.into_bytes().expose())
}

fn publish_private_config(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    // Pairing usually runs as root, while the resident service uses xsoc/_xsoc. If the original configuration
    // already has package-assigned ownership and permissions, atomic replacement must copy them to the new inode.
    // Preserving only mode would leave root:root 0640, unreadable by the service account.
    #[cfg(unix)]
    {
        use xcsc::fs_safety::{ConfigurationDirectory, EntryName};
        let path = std::path::absolute(path)?;
        let directory =
            ConfigurationDirectory::open(path.parent().context("config path has no parent")?)?;
        let name = EntryName::new(path.file_name().context("config path has no filename")?)?;
        directory
            .replace(&name, content)
            .with_context(|| format!("failed to save private config {}", path.display()))
    }
    #[cfg(not(unix))]
    {
        private_fs::write_atomic(path, content)
            .with_context(|| format!("failed to save private config {}", path.display()))
    }
}

fn read_private_config(path: &Path) -> anyhow::Result<SecretBytes> {
    #[cfg(unix)]
    {
        use xcsc::fs_safety::{ConfigurationDirectory, EntryName};
        let path = std::path::absolute(path)?;
        let directory =
            ConfigurationDirectory::open(path.parent().context("config path has no parent")?)?;
        let name = EntryName::new(path.file_name().context("config path has no filename")?)?;
        Ok(SecretBytes::new(
            directory.read_bounded(&name, MAX_CONFIG_BYTES)?,
        ))
    }
    #[cfg(not(unix))]
    {
        Ok(SecretBytes::new(private_fs::read_private(
            path,
            MAX_CONFIG_BYTES,
        )?))
    }
}

fn print_help() {
    println!(
        "xsoc [run|once|probe|pair|doctor|status] [options]\n\
         run   continuously collect and report read-only telemetry (default)\n\
         once  collect and report one snapshot\n\
         probe print the local capability report without contacting a server\n\
         pair  authorize this host in a browser and store a host-scoped credential\n\
         doctor inspect configuration, collection, authorization, and spool without writes\n\
         status print local identity, authorization, pairing, and spool state\n\
         Pairing example:\n\
           xsoc pair --server https://xsos.example.com\n\n\
         Common options: --config PATH [--endpoint REPORT_URL] [--output human|json]\n\
         Delivery requires HTTPS.\n\
         Doctor delivery opt-in: --delivery (sends one report and may drain queued reports)\n\
         Pair options: [--server URL | --endpoint REPORT_URL]\n\
           [--replace-pending-pairing]\n\
         --replace-pending-pairing explicitly abandons an incomplete saved request and\n\
           creates a fresh request with new secrets; browser authorization is required again.\n\
         Remote plaintext HTTP is never accepted by browser pairing.\n\n\
         Browser pairing keeps the long-lived secret local and stores it in the private\n\
         state directory; the browser receives only the public activation status."
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn installer_default_is_valid_under_release_https_policy() {
        let config = super::ClientConfig::default();
        assert!(config.endpoint.starts_with("https://"));
        config.validate(super::ClientCommand::Run).unwrap();
        let serialized = serde_json::to_vec(&config).unwrap();
        let restored: super::ClientConfig = serde_json::from_slice(&serialized).unwrap();
        restored.validate(super::ClientCommand::Run).unwrap();
        super::validate_pairing_endpoint(&restored.pairing_endpoint()).unwrap();
    }

    use super::*;

    #[test]
    fn configuration_secrets_share_storage_and_round_trip_only_at_explicit_fields() {
        let token = Arc::new(SecretString::new("配置令牌🔑".into()));
        let password = Arc::new(SecretString::new(" password with spaces ".into()));
        let config = ClientConfig {
            otlp_token: Some(token.clone()),
            tls_identity_password: Some(password.clone()),
            ..ClientConfig::default()
        };
        let copy = config.clone();
        assert!(Arc::ptr_eq(copy.otlp_token.as_ref().unwrap(), &token));
        assert!(Arc::ptr_eq(
            copy.tls_identity_password.as_ref().unwrap(),
            &password
        ));
        assert!(
            !format!("{:?}/{:?}", copy.otlp_token, copy.tls_identity_password)
                .contains("password with spaces")
        );
        let mut value = serde_json::to_value(&config).unwrap();
        assert_eq!(value["otlp_token"], token.expose());
        assert_eq!(value["tls_identity_password"], password.expose());
        let loaded: ClientConfig = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(loaded.otlp_token.unwrap().expose(), token.expose());
        assert_eq!(
            loaded.tls_identity_password.unwrap().expose(),
            password.expose()
        );
        value.as_object_mut().unwrap().remove("otlp_token");
        value["tls_identity_password"] = serde_json::Value::Null;
        let empty: ClientConfig = serde_json::from_value(value.clone()).unwrap();
        assert!(empty.otlp_token.is_none() && empty.tls_identity_password.is_none());
        value["tls_identity_password"] = serde_json::json!("");
        let empty_password: ClientConfig = serde_json::from_value(value).unwrap();
        assert_eq!(empty_password.tls_identity_password.unwrap().expose(), "");
    }

    #[test]
    fn failed_secret_serialization_preserves_the_saved_configuration() {
        let directory = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!(
                "host-config-secret-budget-{}",
                uuid::Uuid::new_v4()
            ));
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        let path = directory.join("config.json");
        let mut config = ClientConfig {
            config_path: Some(path.clone()),
            otlp_token: Some(Arc::new(SecretString::new("private-token-marker".into()))),
            ..ClientConfig::default()
        };
        config.persist_after_pairing().unwrap();
        let original = fs::read(&path).unwrap();
        assert_eq!(original.last(), Some(&b'\n'));
        let (loaded, _) =
            ClientConfig::load_selected_config(Some(&path), ClientCommand::Run).unwrap();
        assert_eq!(loaded.otlp_token.unwrap().expose(), "private-token-marker");
        // Escaped bytes count toward the budget, not the source String length.
        config.tls_identity_password = Some(Arc::new(SecretString::new(
            "\"".repeat(MAX_CONFIG_BYTES / 2),
        )));
        let error = config.persist_after_pairing().unwrap_err();
        assert!(!format!("{error:#}/{error:?}").contains("private-token-marker"));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_configuration_diagnostics_do_not_echo_secret_values_or_keys() {
        let directory = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!(
                "host-config-secret-errors-{}",
                uuid::Uuid::new_v4()
            ));
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        let path = directory.join("config.json");
        for field in [
            "application_version",
            "otlp_token",
            "tls_identity_password",
            "private-marker-key",
        ] {
            let mut value = serde_json::to_value(ClientConfig::default()).unwrap();
            value[field] = if field == "application_version" {
                serde_json::json!("private-marker-value")
            } else {
                serde_json::json!({"private-marker-value": true})
            };
            let bytes = serde_json::to_vec(&value).unwrap();
            persist_private_config(&path, &bytes).unwrap();
            let error = ClientConfig::load_selected_config(Some(&path), ClientCommand::Run)
                .err()
                .unwrap();
            assert!(!format!("{error:#}/{error:?}").contains("private-marker"));
            let (_, issue) =
                ClientConfig::load_selected_config(Some(&path), ClientCommand::Status).unwrap();
            let issue = issue.unwrap();
            assert!(issue.contains("line") && !issue.contains("private-marker"));
            let mut expected = bytes;
            expected.push(b'\n');
            assert_eq!(fs::read(&path).unwrap(), expected);
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn configuration_persistence_preserves_service_mode_and_rejects_links_and_public_files() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
        let directory = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-config-safety-{}", uuid::Uuid::new_v4()));
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o750)).unwrap();
        let path = directory.join("config.json");
        let config = ClientConfig {
            config_path: Some(path.clone()),
            ..ClientConfig::default()
        };
        config.persist_after_pairing().unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let before = fs::metadata(&path).unwrap();
        config.persist_after_pairing().unwrap();
        let after = fs::metadata(&path).unwrap();
        assert_eq!(
            (after.uid(), after.gid(), after.mode() & 0o7777),
            (before.uid(), before.gid(), 0o640)
        );
        assert!(ClientConfig::load_selected_config(Some(&path), ClientCommand::Run).is_ok());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(config.persist_after_pairing().is_err());
        assert!(ClientConfig::load_selected_config(Some(&path), ClientCommand::Run).is_err());
        let (_, issue) =
            ClientConfig::load_selected_config(Some(&path), ClientCommand::Status).unwrap();
        assert!(issue.is_some());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let victim = directory.join("victim");
        fs::rename(&path, &victim).unwrap();
        let bytes = fs::read(&victim).unwrap();
        symlink(&victim, &path).unwrap();
        assert!(config.persist_after_pairing().is_err());
        assert!(ClientConfig::load_selected_config(Some(&path), ClientCommand::Run).is_err());
        fs::remove_file(&path).unwrap();
        fs::hard_link(&victim, &path).unwrap();
        assert!(config.persist_after_pairing().is_err());
        assert!(ClientConfig::load_selected_config(Some(&path), ClientCommand::Run).is_err());
        assert_eq!(fs::read(&victim).unwrap(), bytes);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn configuration_budget_applies_to_reads_and_writes_without_truncation() {
        let directory = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-config-budget-{}", uuid::Uuid::new_v4()));
        let path = directory.join("config.json");
        assert!(persist_private_config(&path, &vec![b'x'; MAX_CONFIG_BYTES]).is_err());
        assert!(!directory.exists());
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        let mut bytes = serde_json::to_vec(&ClientConfig::default()).unwrap();
        bytes.resize(MAX_CONFIG_BYTES - 1, b' ');
        persist_private_config(&path, &bytes).unwrap();
        assert!(ClientConfig::load_selected_config(Some(&path), ClientCommand::Run).is_ok());
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len((MAX_CONFIG_BYTES + 1) as u64).unwrap();
        assert!(ClientConfig::load_selected_config(Some(&path), ClientCommand::Run).is_err());
        let (_, issue) =
            ClientConfig::load_selected_config(Some(&path), ClientCommand::Status).unwrap();
        assert!(issue.is_some());
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            (MAX_CONFIG_BYTES + 1) as u64
        );
        drop(file);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn spool_configuration_obeys_platform_ceiling() {
        let mut config = ClientConfig {
            spool_max_bytes: xcsc::runtime::MAX_SPOOL_BYTES,
            ..Default::default()
        };
        config.validate_for_diagnostics().unwrap();
        config.spool_max_bytes += 1;
        assert!(
            format!("{:#}", config.validate_for_diagnostics().unwrap_err())
                .contains("invalid spool limits")
        );
    }

    #[test]
    fn package_version_output_is_exact() {
        assert_eq!(
            client_version_output(),
            concat!("xsoc ", env!("CARGO_PKG_VERSION"))
        );
        #[cfg(target_os = "linux")]
        assert_eq!(LINUX_PACKAGE_VERSION_MARKER.last(), Some(&0));
    }

    #[test]
    fn selecting_more_than_one_command_is_rejected() {
        let mut selected = None;
        select_command(&mut selected, ClientCommand::Run, "run").unwrap();
        let error = select_command(&mut selected, ClientCommand::Probe, "probe")
            .expect_err("a second command must not override the first one");
        assert!(error.to_string().contains("multiple commands"));
        assert_eq!(selected, Some(ClientCommand::Run));
    }

    #[test]
    fn explicit_missing_or_non_regular_config_is_lenient_only_for_status() {
        let root = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("xsoc-explicit-config-{}", uuid::Uuid::new_v4()));
        let directory = root.join("directory-config");
        fs::create_dir_all(&directory).unwrap();
        let missing = root.join("missing-config.json");

        for path in [&missing, &directory] {
            for command in [
                ClientCommand::Run,
                ClientCommand::Once,
                ClientCommand::Probe,
                ClientCommand::Pair,
                ClientCommand::Doctor,
            ] {
                let error = ClientConfig::load_selected_config(Some(path), command)
                    .err()
                    .expect("an explicit unusable config must stop non-status commands");
                assert!(format!("{error:#}").contains(&path.display().to_string()));
            }

            let (config, issue) =
                ClientConfig::load_selected_config(Some(path), ClientCommand::Status)
                    .expect("status must remain available for configuration diagnostics");
            assert_eq!(
                config.endpoint,
                format!(
                    "{DEFAULT_SERVER_ORIGIN}{}",
                    xsos_protocol::CLIENT_REPORT_PATH
                )
            );
            assert!(
                issue
                    .as_deref()
                    .is_some_and(|message| message.contains(&path.display().to_string()))
            );
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn windows_service_flag_is_platform_gated() {
        assert!(validate_windows_service_invocation(false, ClientCommand::Run).is_ok());
        #[cfg(not(windows))]
        assert!(validate_windows_service_invocation(true, ClientCommand::Run).is_err());
        #[cfg(windows)]
        {
            assert!(validate_windows_service_invocation(true, ClientCommand::Run).is_ok());
            assert!(validate_windows_service_invocation(true, ClientCommand::Probe).is_err());
        }
    }

    #[test]
    fn windows_service_flag_must_be_the_first_user_argument() {
        assert!(validate_windows_service_position(1).is_ok());
        assert!(validate_windows_service_position(2).is_err());
        assert!(validate_windows_service_position(3).is_err());
    }

    #[test]
    fn derives_pairing_endpoint_from_the_current_report_endpoint() {
        let config = ClientConfig {
            endpoint: "https://xsos.example/prefix/api/v1/xsoc/report".into(),
            ..ClientConfig::default()
        };
        assert_eq!(
            config.pairing_endpoint(),
            "https://xsos.example/prefix/api/v1/xsoc/pairing-requests"
        );
    }

    #[test]
    fn packaged_example_server_is_never_treated_as_a_real_network_target() {
        let config = ClientConfig {
            endpoint: format!(
                "https://xsos.example.com{}",
                xsos_protocol::CLIENT_REPORT_PATH
            ),
            pairing_endpoint: Some(format!(
                "https://xsos.example.com{}",
                xsos_protocol::CLIENT_PAIRING_REQUESTS_PATH
            )),
            ..ClientConfig::default()
        };
        assert!(config.uses_packaged_placeholder_server());
        assert!(!ClientConfig::default().uses_packaged_placeholder_server());
    }

    #[test]
    fn pairing_server_must_be_a_root_management_console_origin() {
        let mut root = ClientConfig {
            server_override: Some("https://xsos.example/".into()),
            ..ClientConfig::default()
        };
        root.apply_pair_options().unwrap();
        assert_eq!(
            root.pairing_endpoint.as_deref(),
            Some("https://xsos.example/api/v1/xsoc/pairing-requests")
        );

        let mut path = ClientConfig {
            server_override: Some("https://xsos.example/console".into()),
            ..ClientConfig::default()
        };
        let error = path
            .apply_pair_options()
            .expect_err("a path would silently target the wrong pairing API");
        assert!(format!("{error:#}").contains("without a path"));
    }

    #[test]
    fn rejects_remote_plaintext_by_default() {
        assert!(validate_endpoint("http://192.0.2.10/report").is_err());
        assert!(validate_endpoint("http://127.0.0.1/report").is_err());
        assert!(validate_endpoint("http://[::1]/report").is_err());
        assert!(validate_endpoint("https://telemetry.example/report").is_ok());
    }

    #[test]
    fn pairing_endpoint_rejects_query_and_fragment_without_restricting_telemetry() {
        for endpoint in [
            "https://xsos.example/api/v1/xsoc/pairing-requests?tenant=one",
            "https://xsos.example/api/v1/xsoc/pairing-requests#bootstrap",
            "https://xsos.example/api/v1/xsoc/pairing-requests?#",
        ] {
            let config = ClientConfig {
                pairing_endpoint: Some(endpoint.into()),
                ..ClientConfig::default()
            };
            let error = config
                .validate(ClientCommand::Run)
                .expect_err("pairing request paths cannot be appended after a query or fragment");
            let message = format!("{error:#}");
            assert!(message.contains("query or fragment") || message.contains("trusted HTTPS"));
        }

        assert!(validate_endpoint("https://telemetry.example/report?tenant=one").is_ok());
        assert!(
            validate_endpoint("https://telemetry.example/report#client").is_err(),
            "xcsc rejects URL fragments before any network request"
        );
    }

    #[test]
    fn local_diagnostics_are_not_blocked_by_a_bad_network_endpoint() {
        let config = ClientConfig {
            endpoint: "not a URL".into(),
            ..ClientConfig::default()
        };
        assert!(config.validate(ClientCommand::Status).is_ok());
        assert!(config.validate(ClientCommand::Probe).is_ok());
        assert!(config.validate(ClientCommand::Doctor).is_ok());
        assert!(config.validate(ClientCommand::Run).is_err());
    }

    #[test]
    fn doctor_delivery_explicitly_restores_network_validation() {
        let config = ClientConfig {
            endpoint: "not a URL".into(),
            doctor_delivery: true,
            ..ClientConfig::default()
        };
        assert!(config.validate(ClientCommand::Doctor).is_err());
    }

    #[test]
    fn tls_identity_password_requires_a_pkcs12_identity() {
        let config = ClientConfig {
            tls_identity_password: Some(Arc::new(SecretString::new("secret".into()))),
            ..ClientConfig::default()
        };
        let error = config
            .validate(ClientCommand::Run)
            .expect_err("an otherwise unused TLS identity password must not be ignored");
        assert!(error.to_string().contains("tls_identity_pkcs12"));
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    #[test]
    fn non_native_tls_backend_rejects_pkcs12_identity() {
        let config = ClientConfig {
            tls_identity_pkcs12: Some("client-identity.p12".into()),
            ..ClientConfig::default()
        };
        let error = config
            .validate(ClientCommand::Run)
            .expect_err("an unsupported PKCS#12 identity must not be silently ignored");
        assert!(error.to_string().contains("tls_identity_pem"));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn native_tls_backend_rejects_pem_identity() {
        let config = ClientConfig {
            tls_identity_pem: Some("client-identity.pem".into()),
            ..ClientConfig::default()
        };
        let error = config
            .validate(ClientCommand::Run)
            .expect_err("an unsupported PEM identity must not reach request construction");
        assert!(error.to_string().contains("tls_identity_pkcs12"));
    }

    #[test]
    fn removed_pre_pairing_configuration_fields_are_rejected() {
        for field in [
            "registration_endpoint",
            "token",
            "enrollment_token",
            "host_id",
            "host_name",
        ] {
            let mut document = serde_json::to_value(ClientConfig::default()).unwrap();
            document
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), serde_json::Value::Null);
            assert!(
                serde_json::from_value::<ClientConfig>(document).is_err(),
                "unknown field {field} must not be silently accepted"
            );
        }
    }

    #[test]
    fn configuration_requires_the_frozen_format_and_shape() {
        let current = serde_json::to_value(ClientConfig::default()).unwrap();
        assert_eq!(current["application_version"], CONFIG_FORMAT_VERSION);
        serde_json::from_value::<ClientConfig>(current.clone()).unwrap();

        let mut legacy = current.clone();
        legacy["application_version"] = serde_json::json!("0.9.3");
        assert!(serde_json::from_value::<ClientConfig>(legacy).is_err());

        let mut missing_version = current.clone();
        missing_version
            .as_object_mut()
            .unwrap()
            .remove("application_version");
        assert!(serde_json::from_value::<ClientConfig>(missing_version).is_err());

        let mut different_version = current.clone();
        different_version["application_version"] = serde_json::json!("not-current");
        assert!(serde_json::from_value::<ClientConfig>(different_version).is_err());

        let mut incomplete = current;
        incomplete
            .as_object_mut()
            .unwrap()
            .remove("interval_seconds");
        assert!(serde_json::from_value::<ClientConfig>(incomplete).is_err());
    }

    fn config_with_interval(interval_seconds: u64) -> ClientConfig {
        ClientConfig {
            interval_seconds,
            // slow_interval must be >= interval or another validation rule will fail first.
            slow_interval_seconds: interval_seconds,
            ..ClientConfig::default()
        }
    }

    /// The contract upper bound constrains the measured cycle. Delivery is decoupled, so the normal cycle depends only on
    /// ticker jitter. With nonzero default jitter, a base interval equal to the upper bound necessarily exceeds it.
    ///
    /// Startup validation must reject upper-bound intervals with jitter so every runtime report satisfies the contract.
    #[test]
    fn rejects_interval_at_the_contract_limit_because_jitter_pushes_it_over() {
        let config = config_with_interval(MAX_REPORT_INTERVAL_SECONDS);
        assert!(
            config.jitter_percent > 0,
            "this case requires nonzero default jitter"
        );
        let error = config
            .validate(ClientCommand::Run)
            .expect_err("an upper-bound interval with jitter necessarily exceeds the contract and must be rejected");
        let message = error.to_string();
        assert!(
            message.contains("3600") && message.contains("400"),
            "the error must explain the bound and consequences; received: {message}"
        );
    }

    /// Rejection must suggest a usable alternative value instead of requiring trial and error.
    #[test]
    fn the_reported_maximum_interval_is_actually_accepted() {
        let rejected = config_with_interval(MAX_REPORT_INTERVAL_SECONDS);
        let suggested = rejected.max_interval_seconds_at_current_jitter();
        let config = config_with_interval(suggested);
        assert!(
            config.validate(ClientCommand::Run).is_ok(),
            "the suggested interval_seconds={suggested} in the error must pass validation"
        );
        assert!(
            config.worst_case_cycle_seconds() <= MAX_REPORT_INTERVAL_SECONDS as f64,
            "the suggested value must keep the worst-case cycle within the contract"
        );
    }

    /// With zero jitter, ticker cadence equals the configured interval, which may reach the upper bound.
    /// Collection-side clamping handles abnormal elapsed time after sleep or process suspension.
    #[test]
    fn zero_jitter_allows_the_full_contract_range() {
        let config = ClientConfig {
            jitter_percent: 0,
            ..config_with_interval(MAX_REPORT_INTERVAL_SECONDS)
        };
        assert!(config.validate(ClientCommand::Run).is_ok());
    }

    #[test]
    fn rejects_two_hour_interval_that_would_be_silently_dropped() {
        assert!(
            config_with_interval(7200)
                .validate(ClientCommand::Run)
                .is_err()
        );
    }

    #[test]
    fn request_timeout_is_bounded_for_graceful_process_cancellation() {
        let mut config = config_with_interval(10);
        config.request_timeout_seconds = 300;
        assert!(config.validate(ClientCommand::Run).is_ok());
        config.request_timeout_seconds = 301;
        let error = config
            .validate(ClientCommand::Run)
            .expect_err("unbounded network waits defeat process cancellation guarantees");
        assert!(error.to_string().contains("300"));
    }

    #[cfg(not(feature = "otlp"))]
    #[test]
    fn configured_otlp_requires_the_optional_feature() {
        let config = ClientConfig {
            otlp_endpoint: Some("https://collector.example/v1/metrics".into()),
            ..config_with_interval(10)
        };
        let error = config
            .validate(ClientCommand::Run)
            .expect_err("a non-OTLP build must not silently ignore configured export");
        assert!(error.to_string().contains("--features otlp"));
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn optional_otlp_feature_accepts_a_valid_endpoint() {
        let config = ClientConfig {
            otlp_endpoint: Some("https://collector.example/v1/metrics".into()),
            ..config_with_interval(10)
        };
        assert!(config.validate(ClientCommand::Run).is_ok());
    }

    /// Collection-side safeguards must keep measured intervals within the server contract.
    ///
    /// The server permanently rejects reports below the lower bound with 400 and acknowledges their removal from spool. Use
    /// the most extreme permitted configuration to verify this boundary:
    /// the smallest valid interval (1 second) with maximum jitter must still yield a cycle above the contract lower bound.
    #[test]
    fn the_shortest_possible_cycle_stays_inside_the_server_contract() {
        let smallest_interval = 1.0_f64; // validate() requires interval_seconds >= 1
        let largest_jitter = 50.0_f64 / 100.0; // validate() requires jitter_percent <= 50
        let shortest_cycle = smallest_interval * (1.0 - largest_jitter);

        assert!(
            shortest_cycle > MIN_REPORT_INTERVAL_SECONDS,
            "the shortest possible cycle {shortest_cycle}s approaches the contract lower bound {MIN_REPORT_INTERVAL_SECONDS}s;\
             reassess this boundary when changing maximum jitter or the minimum interval"
        );
    }
}
