#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrayPreferences {
    application_version: CurrentPackageVersion,
    server: String,
}

#[derive(Clone)]
struct ExpiringToken {
    value: String,
    expires: Instant,
}

#[derive(Clone)]
struct BrowserSession {
    bearer: String,
    expires: Instant,
}

struct LocalControlState {
    bootstrap_tokens: Mutex<Vec<ExpiringToken>>,
    sessions: Mutex<Vec<BrowserSession>>,
    operations: Mutex<Vec<BrowserOperation>>,
    active_pairings: AtomicUsize,
    active_service_operations: AtomicUsize,
    preferences_path: PathBuf,
}

#[derive(Clone, Serialize)]
struct BrowserOperation {
    id: String,
    kind: &'static str,
    phase: &'static str,
    message: String,
    terminal: bool,
    success: Option<bool>,
}

fn create_operation(
    state: &Arc<LocalControlState>,
    kind: &'static str,
    phase: &'static str,
    message: impl Into<String>,
) -> String {
    let operation = BrowserOperation {
        id: random_secret(),
        kind,
        phase,
        message: message.into(),
        terminal: false,
        success: None,
    };
    let id = operation.id.clone();
    let mut operations = lock(&state.operations);
    if operations.len() >= 32 {
        let remove = operations
            .iter()
            .position(|operation| operation.terminal)
            .unwrap_or(0);
        operations.remove(remove);
    }
    operations.push(operation);
    id
}

fn update_operation(
    state: &Arc<LocalControlState>,
    id: &str,
    phase: &'static str,
    message: impl Into<String>,
    outcome: Option<bool>,
) {
    if let Some(operation) = lock(&state.operations)
        .iter_mut()
        .find(|operation| operation.id == id)
    {
        operation.phase = phase;
        operation.message = message.into();
        operation.terminal = outcome.is_some();
        operation.success = outcome;
    }
}

struct LocalControlServer {
    origin: String,
    state: Arc<LocalControlState>,
}

struct ActiveConnection(Arc<AtomicUsize>);

impl Drop for ActiveConnection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct PairingSlot(Arc<LocalControlState>);

fn claim_pairing_slot(state: &Arc<LocalControlState>) -> anyhow::Result<PairingSlot> {
    ensure!(
        state
            .active_pairings
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok(),
        "配对操作正在进行中，请等待其完成后再试"
    );
    Ok(PairingSlot(Arc::clone(state)))
}

impl Drop for PairingSlot {
    fn drop(&mut self) {
        self.0.active_pairings.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ServiceOperationSlot(Arc<LocalControlState>);

fn claim_service_operation(state: &Arc<LocalControlState>) -> anyhow::Result<ServiceOperationSlot> {
    ensure!(
        state
            .active_service_operations
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok(),
        "服务操作正在进行中，请等待其完成后再试"
    );
    Ok(ServiceOperationSlot(Arc::clone(state)))
}

impl Drop for ServiceOperationSlot {
    fn drop(&mut self) {
        self.0
            .active_service_operations
            .fetch_sub(1, Ordering::AcqRel);
    }
}

struct SensitiveActivationCode(Vec<u8>);

impl SensitiveActivationCode {
    fn new(value: String) -> Self {
        Self(value.into_bytes())
    }

    fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SensitiveActivationCode {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

struct PairIpcServer {
    pipe: TransferHandle,
}

impl PairIpcServer {
    fn create(nonce: &str) -> anyhow::Result<Self> {
        validate_callback_nonce(nonce)?;
        let name = wide(&pair_pipe_name(nonce));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:P(A;;GA;;;BA)(A;;GA;;;SY)"),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .context("failed to create the protected pairing pipe descriptor")?;
        let _descriptor = LocalSecurityDescriptor(descriptor);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let pipe = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                MAX_LOCAL_HTTP_BODY_BYTES as u32,
                MAX_LOCAL_HTTP_BODY_BYTES as u32,
                0,
                Some(&attributes),
            )
        };
        ensure!(
            !pipe.is_invalid(),
            "failed to create the protected pairing pipe: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self {
            pipe: TransferHandle::new(KernelHandle(pipe)),
        })
    }

    fn serve(
        self,
        process: TransferHandle,
        server: &str,
        activation_code: SensitiveActivationCode,
    ) -> anyhow::Result<PairingChildReconciliation> {
        let pipe = self.pipe.into_kernel();
        let process = process.into_kernel();
        let expected_pid = unsafe { GetProcessId(process.0) };
        ensure!(
            expected_pid != 0,
            "failed to identify the elevated pairing broker"
        );
        let deadline = Instant::now() + PAIR_OPERATION_TIMEOUT;
        connect_pipe_with_deadline(pipe.0, process.0, deadline)?;

        let mut client_pid = 0_u32;
        unsafe { GetNamedPipeClientProcessId(pipe.0, &mut client_pid) }
            .context("failed to identify the pairing pipe client")?;
        ensure!(
            client_pid == expected_pid,
            "pairing pipe client is not the broker launched by this tray"
        );
        let message =
            read_pipe_frame(pipe.0, MAX_LOCAL_HTTP_BODY_BYTES, deadline, Some(process.0))?;
        let message: PairIpcMessage =
            serde_json::from_slice(&message).context("invalid pairing pipe message")?;
        validate_pair_ipc_message(&message, server)?;
        write_pipe_frame(
            pipe.0,
            activation_code.as_bytes(),
            deadline,
            Some(process.0),
        )?;
        // The pipe and Client stdin now own the transient transport copies;
        // erase the standard tray's retained allocation before the long
        // activation/poll wait.
        drop(activation_code);

        // Sending the key is only the midpoint of pairing: the Client still
        // has to activate, poll, commit credentials, and the broker may
        // need to restart the service. Keep the standard-user pairing slot
        // until this exact ShellExecuteEx process exits successfully so a
        // second click cannot launch another UAC prompt mid-transaction.
        let process_deadline = Instant::now() + PAIR_OPERATION_TIMEOUT + PAIR_BROKER_EXIT_GRACE;
        let completion = read_pipe_frame(
            pipe.0,
            MAX_LOCAL_HTTP_BODY_BYTES,
            process_deadline,
            Some(process.0),
        )?;
        let completion: PairingChildReconciliation = serde_json::from_slice(&completion)
            .context("invalid pairing completion IPC message")?;
        validate_pairing_child_reconciliation(&completion)?;
        let remaining = process_deadline.saturating_duration_since(Instant::now());
        let wait_millis = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX - 1);
        let wait = unsafe { WaitForSingleObject(process.0, wait_millis) };
        ensure!(
            wait == WAIT_OBJECT_0,
            "timed out or failed while waiting for the elevated pairing broker ({wait:?})"
        );
        let mut exit_code = u32::MAX;
        unsafe { GetExitCodeProcess(process.0, &mut exit_code) }
            .context("failed to inspect the elevated pairing broker result")?;
        ensure!(
            exit_code == 0,
            "elevated pairing broker failed with exit code {exit_code}"
        );
        Ok(completion)
    }
}

struct LocalSecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
        }
    }
}

#[derive(Debug)]
struct LocalHttpRequest {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct HttpResponse {
    status: &'static str,
    content_type: &'static str,
    extra_headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairRequest {
    server: String,
    activation_code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceRequest {
    action: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationRequest {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateRequest {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionRequest {
    server: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PairIpcMessage {
    generation: String,
    request_id: String,
    activation_url: String,
    pairing_endpoint: String,
}

#[derive(Debug, Clone, Copy, Default)]
struct CurrentPackageVersion;

impl Serialize for CurrentPackageVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(env!("CARGO_PKG_VERSION"))
    }
}

impl<'de> Deserialize<'de> for CurrentPackageVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let version = String::deserialize(deserializer)?;
        if version == env!("CARGO_PKG_VERSION") {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(format!(
                "pairing event belongs to Client {version}, expected {}",
                env!("CARGO_PKG_VERSION")
            )))
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
enum PairEvent {
    #[serde(rename = "pairing_waiting")]
    PairingWaiting {
        #[serde(rename = "version")]
        _version: CurrentPackageVersion,
        generation: String,
        request_id: String,
        activation_url: String,
        pairing_endpoint: String,
        #[serde(rename = "expires_at")]
        _expires_at: String,
        #[serde(rename = "poll_interval")]
        _poll_interval: u64,
    },
    #[serde(rename = "paired")]
    Paired {
        #[serde(rename = "version")]
        _version: CurrentPackageVersion,
        request_id: String,
        instance_id: String,
        #[serde(rename = "endpoint")]
        _endpoint: String,
    },
    #[serde(rename = "pairing_interrupted")]
    PairingInterrupted {
        #[serde(rename = "version")]
        _version: CurrentPackageVersion,
        #[serde(rename = "request_id")]
        _request_id: String,
    },
    #[serde(rename = "pairing_cancelled")]
    PairingCancelled {
        #[serde(rename = "version")]
        _version: CurrentPackageVersion,
    },
    #[serde(rename = "pairing_timeout")]
    PairingTimeout {
        #[serde(rename = "version")]
        _version: CurrentPackageVersion,
    },
}
