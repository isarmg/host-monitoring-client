#[cfg(any(windows, test))]
use anyhow::{Context, bail, ensure};

#[cfg(any(windows, test))]
const MAINTENANCE_DIAGNOSTIC_MAX_BYTES: usize = 64 * 1024;
#[cfg(any(windows, test))]
const MAINTENANCE_DIAGNOSTIC_FORMAT: &str = "host-monitor-maintenance-diagnostic-v1";
#[cfg(any(windows, test))]
const MAINTENANCE_DIAGNOSTIC_SDDL: &str = "O:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)";

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
struct MaintenanceInvocation {
    command: String,
    diagnostics: bool,
}

#[cfg(any(windows, test))]
fn is_maintenance_command(command: &str) -> bool {
    matches!(
        command,
        "prepare-install"
            | "apply-install"
            | "rollback-install"
            | "commit-install"
            | "preflight-uninstall"
            | "rollback-uninstall-preflight"
            | "preserve-state"
            | "rollback-uninstall"
            | "commit-uninstall"
            | "prepare-purge"
            | "rollback-purge"
            | "commit-purge"
    )
}

#[cfg(any(windows, test))]
fn parse_maintenance_arguments(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> anyhow::Result<MaintenanceInvocation> {
    let mut arguments = arguments.into_iter();
    let command = arguments
        .next()
        .context("expected one maintenance command")?;
    let command = command
        .to_str()
        .context("maintenance command must be valid Unicode")?
        .to_owned();
    ensure!(
        is_maintenance_command(&command),
        "unknown maintenance command"
    );
    let diagnostics = match arguments.next() {
        None => false,
        Some(value) => {
            ensure!(
                value.to_str() == Some("1"),
                "maintenance diagnostics flag must be the exact Unicode value 1"
            );
            true
        }
    };
    ensure!(
        arguments.next().is_none(),
        "maintenance commands accept only the command and optional diagnostics flag"
    );
    Ok(MaintenanceInvocation {
        command,
        diagnostics,
    })
}

#[cfg(any(windows, test))]
struct BoundedDiagnosticPayload {
    bytes: Vec<u8>,
    max_bytes: usize,
    truncated: bool,
}

#[cfg(any(windows, test))]
impl BoundedDiagnosticPayload {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
            truncated: false,
        }
    }
}

#[cfg(any(windows, test))]
impl std::fmt::Write for BoundedDiagnosticPayload {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        let remaining = self.max_bytes.saturating_sub(self.bytes.len());
        if remaining == 0 {
            self.truncated |= !value.is_empty();
            return Ok(());
        }
        let mut accepted = value.len().min(remaining);
        while !value.is_char_boundary(accepted) {
            accepted -= 1;
        }
        self.bytes.extend_from_slice(&value.as_bytes()[..accepted]);
        self.truncated |= accepted != value.len();
        Ok(())
    }
}

#[cfg(any(windows, test))]
fn maintenance_diagnostic_payload(command: &str, error: &anyhow::Error) -> Vec<u8> {
    use std::fmt::Write;

    let mut payload = BoundedDiagnosticPayload::new(MAINTENANCE_DIAGNOSTIC_MAX_BYTES);
    let _ = write!(
        payload,
        "format={MAINTENANCE_DIAGNOSTIC_FORMAT}\nversion={}\ncommand={command}\nerror-chain={error:#}\n",
        env!("CARGO_PKG_VERSION")
    );
    if !payload.truncated {
        return payload.bytes;
    }

    // Preserve the innermost cause when an unusually large outer context does
    // not fit. The explicit marker keeps a bounded payload from masquerading
    // as a complete anyhow chain. Reserve one byte for a final newline so a
    // truncated UTF-8 value still produces a complete text record.
    let mut payload = BoundedDiagnosticPayload::new(MAINTENANCE_DIAGNOSTIC_MAX_BYTES - 1);
    let _ = write!(
        payload,
        "format={MAINTENANCE_DIAGNOSTIC_FORMAT}\nversion={}\ncommand={command}\nerror-chain=[truncated]\ntruncated=true\nleaf-cause={}\nouter-context={error}",
        env!("CARGO_PKG_VERSION"),
        error.root_cause()
    );
    payload.bytes.push(b'\n');
    payload.bytes
}

#[cfg(any(windows, test))]
fn checked_rollback_path_status(
    path: &std::path::Path,
    label: &str,
    status: std::io::Result<bool>,
) -> anyhow::Result<bool> {
    status.with_context(|| {
        format!(
            "failed to inspect {label} at {} before rollback",
            path.display()
        )
    })
}

#[cfg(windows)]
fn rollback_path_exists(path: &std::path::Path, label: &str) -> anyhow::Result<bool> {
    checked_rollback_path_status(path, label, path.try_exists())
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug)]
struct OpenedManagedTargetFacts {
    expected_directory: bool,
    actual_directory: bool,
    is_reparse_point: bool,
    hard_link_count: u32,
}

#[cfg(any(windows, test))]
fn validate_opened_managed_target_facts(facts: OpenedManagedTargetFacts) -> anyhow::Result<()> {
    ensure!(
        !facts.is_reparse_point,
        "managed target handle refers to a reparse point"
    );
    ensure!(
        facts.actual_directory == facts.expected_directory,
        "managed target changed type while it was being opened"
    );
    ensure!(
        facts.actual_directory || facts.hard_link_count == 1,
        "managed target handle refers to a multiply linked file"
    );
    Ok(())
}

#[cfg(any(windows, test))]
const MAX_OPEN_MUTATION_DIRECTORIES: usize = 256;

#[cfg(any(windows, test))]
fn checked_child_directory_depth(parent_depth: usize, hard_limit: usize) -> anyhow::Result<usize> {
    let child_depth = parent_depth
        .checked_add(1)
        .context("managed directory depth overflowed")?;
    ensure!(
        child_depth <= hard_limit,
        "managed directory depth exceeds the hard limit of {hard_limit} open handles"
    );
    Ok(child_depth)
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RenameBufferPlan {
    file_name_bytes: u32,
    buffer_bytes: u32,
    storage_words: usize,
}

#[cfg(any(windows, test))]
fn checked_rename_buffer_plan(
    file_name_utf16_units: usize,
    header_bytes: usize,
    storage_word_bytes: usize,
    hard_limit: usize,
) -> anyhow::Result<RenameBufferPlan> {
    ensure!(
        file_name_utf16_units > 0,
        "rename destination must not be empty"
    );
    ensure!(
        storage_word_bytes > 0,
        "rename buffer storage alignment must not be zero"
    );
    let file_name_bytes = file_name_utf16_units
        .checked_mul(std::mem::size_of::<u16>())
        .context("rename destination UTF-16 byte count overflowed")?;
    let buffer_bytes = header_bytes
        .checked_add(file_name_bytes)
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<u16>()))
        .context("rename information buffer size overflowed")?;
    ensure!(
        buffer_bytes <= hard_limit,
        "rename information exceeds the {hard_limit}-byte hard limit"
    );
    let storage_words = buffer_bytes
        .checked_add(storage_word_bytes - 1)
        .context("rename information storage size overflowed")?
        / storage_word_bytes;
    Ok(RenameBufferPlan {
        file_name_bytes: u32::try_from(file_name_bytes)
            .context("rename destination byte count does not fit in a DWORD")?,
        buffer_bytes: u32::try_from(buffer_bytes)
            .context("rename information buffer size does not fit in a DWORD")?,
        storage_words,
    })
}

#[cfg(any(windows, test))]
fn checked_rename_storage_bytes(
    plan: RenameBufferPlan,
    storage_word_bytes: usize,
    rust_struct_bytes: usize,
    file_name_offset: usize,
) -> anyhow::Result<usize> {
    let allocated_bytes = plan
        .storage_words
        .checked_mul(storage_word_bytes)
        .context("rename information allocation size overflowed")?;
    let file_name_bytes = usize::try_from(plan.file_name_bytes)
        .context("rename destination byte count does not fit in usize")?;
    let populated_bytes = file_name_offset
        .checked_add(file_name_bytes)
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<u16>()))
        .context("rename information populated size overflowed")?;
    ensure!(
        u32::try_from(populated_bytes)
            .context("rename information populated size does not fit in a DWORD")?
            == plan.buffer_bytes,
        "rename information plan is internally inconsistent"
    );
    ensure!(
        allocated_bytes >= rust_struct_bytes,
        "rename information allocation is smaller than FILE_RENAME_INFO"
    );
    ensure!(
        allocated_bytes >= populated_bytes,
        "rename information allocation is smaller than its flexible filename tail"
    );
    Ok(allocated_bytes)
}

/// State can legitimately contain the bounded report spool plus package files,
/// but maintenance must never materialize an attacker-sized tree in memory.
#[cfg(any(windows, test))]
const MAX_MAINTENANCE_TREE_NODES: usize = 16 * 1024;
#[cfg(any(windows, test))]
const MAX_ACL_SNAPSHOT_ENTRIES: usize = MAX_MAINTENANCE_TREE_NODES;
#[cfg(any(windows, test))]
const MAX_ACL_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
#[cfg(any(windows, test))]
const MAX_MAINTENANCE_PATH_BYTES: usize = 32 * 1024 * 1024;
#[cfg(any(windows, test))]
const MAINTENANCE_PATH_FIXED_BYTES: usize = 64;
/// Conservatively charge the entry struct, allocator bookkeeping, and JSON
/// field/punctuation overhead in addition to its two variable payloads.
#[cfg(any(windows, test))]
const ACL_SNAPSHOT_ENTRY_FIXED_BYTES: usize = 128;

#[cfg(any(windows, test))]
fn checked_acl_snapshot_entry_payload(
    accounted_bytes: usize,
    relative_path_utf16_units: usize,
    sddl_bytes: usize,
    hard_limit: usize,
) -> anyhow::Result<usize> {
    let path_bytes = relative_path_utf16_units
        .checked_mul(std::mem::size_of::<u16>())
        .context("ACL snapshot UTF-16 path byte count overflowed")?;
    let entry_bytes = ACL_SNAPSHOT_ENTRY_FIXED_BYTES
        .checked_add(path_bytes)
        .and_then(|bytes| bytes.checked_add(sddl_bytes))
        .context("ACL snapshot entry payload byte count overflowed")?;
    let requested = accounted_bytes
        .checked_add(entry_bytes)
        .context("ACL snapshot cumulative payload byte count overflowed")?;
    ensure!(
        requested <= hard_limit,
        "ACL snapshot entry payload exceeds the {hard_limit}-byte cumulative hard limit"
    );
    Ok(requested)
}

#[cfg(any(windows, test))]
fn try_reserve_bounded<T>(
    items: &mut Vec<T>,
    additional: usize,
    hard_limit: usize,
    label: &str,
) -> anyhow::Result<()> {
    let requested = items
        .len()
        .checked_add(additional)
        .context("bounded maintenance collection length overflowed")?;
    ensure!(
        requested <= hard_limit,
        "{label} exceeded the hard limit of {hard_limit} entries"
    );
    items.try_reserve(additional).map_err(|error| {
        anyhow::anyhow!(
            "failed to reserve memory for {label} within the {hard_limit}-entry hard limit: {error}"
        )
    })
}

#[cfg(any(windows, test))]
// Keep both independent caps and their mutable accounting explicit at each call site.
#[allow(clippy::too_many_arguments)]
fn try_push_bounded_acl_snapshot_entry<T>(
    entries: &mut Vec<T>,
    entry: T,
    relative_path_utf16_units: usize,
    sddl_bytes: usize,
    accounted_payload_bytes: &mut usize,
    entry_limit: usize,
    payload_byte_limit: usize,
    label: &str,
) -> anyhow::Result<()> {
    let requested_payload_bytes = checked_acl_snapshot_entry_payload(
        *accounted_payload_bytes,
        relative_path_utf16_units,
        sddl_bytes,
        payload_byte_limit,
    )?;
    try_reserve_bounded(entries, 1, entry_limit, label)?;
    entries.push(entry);
    *accounted_payload_bytes = requested_payload_bytes;
    Ok(())
}

#[cfg(any(windows, test))]
fn checked_maintenance_path_payload(
    accounted_bytes: usize,
    path_utf16_units: usize,
    hard_limit: usize,
    label: &str,
) -> anyhow::Result<usize> {
    let path_bytes = path_utf16_units
        .checked_mul(std::mem::size_of::<u16>())
        .context("maintenance UTF-16 path byte count overflowed")?;
    let entry_bytes = MAINTENANCE_PATH_FIXED_BYTES
        .checked_add(path_bytes)
        .context("maintenance path payload byte count overflowed")?;
    let requested = accounted_bytes
        .checked_add(entry_bytes)
        .context("maintenance cumulative path byte count overflowed")?;
    ensure!(
        requested <= hard_limit,
        "{label} exceeded the {hard_limit}-byte cumulative path hard limit"
    );
    Ok(requested)
}

#[cfg(any(windows, test))]
fn try_push_bounded_path<T>(
    items: &mut Vec<T>,
    item: T,
    path_utf16_units: usize,
    accounted_path_bytes: &mut usize,
    entry_limit: usize,
    path_byte_limit: usize,
    label: &str,
) -> anyhow::Result<()> {
    let requested_path_bytes = checked_maintenance_path_payload(
        *accounted_path_bytes,
        path_utf16_units,
        path_byte_limit,
        label,
    )?;
    try_reserve_bounded(items, 1, entry_limit, label)?;
    items.push(item);
    *accounted_path_bytes = requested_path_bytes;
    Ok(())
}

#[cfg(any(windows, test))]
fn record_bounded_tree_node(
    discovered: &mut usize,
    hard_limit: usize,
    label: &str,
) -> anyhow::Result<()> {
    ensure!(
        *discovered < hard_limit,
        "{label} exceeded the hard limit of {hard_limit} nodes"
    );
    *discovered += 1;
    Ok(())
}

#[cfg(any(windows, test))]
// Keep the node and path budgets visible beside both mutable counters.
#[allow(clippy::too_many_arguments)]
fn enqueue_bounded_tree_path<T>(
    pending: &mut Vec<T>,
    discovered: &mut usize,
    node: T,
    path_utf16_units: usize,
    accounted_path_bytes: &mut usize,
    hard_limit: usize,
    path_byte_limit: usize,
    label: &str,
) -> anyhow::Result<()> {
    ensure!(
        *discovered < hard_limit,
        "{label} exceeded the hard limit of {hard_limit} nodes"
    );
    try_push_bounded_path(
        pending,
        node,
        path_utf16_units,
        accounted_path_bytes,
        hard_limit,
        path_byte_limit,
        label,
    )?;
    *discovered += 1;
    Ok(())
}

#[cfg(any(windows, test))]
fn read_file_bounded(
    path: &std::path::Path,
    max_bytes: usize,
    label: &str,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read;

    let read_limit = max_bytes
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("bounded file read limit overflowed"))?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(u64::try_from(read_limit).map_err(std::io::Error::other)?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{label} exceeds the {max_bytes}-byte hard limit"),
        ));
    }
    Ok(bytes)
}

#[cfg(any(windows, test))]
#[derive(Clone, Debug)]
struct AclSnapshotPathFact {
    path_key: Vec<u16>,
    depth: usize,
    valid_relative_path: bool,
    is_directory: bool,
}

#[cfg(any(windows, test))]
#[derive(Clone, Debug)]
struct AclCurrentPathFact {
    path_key: Vec<u16>,
    is_directory: bool,
    is_regular_file: bool,
    is_reparse_point: bool,
    hard_link_count: Option<u64>,
}

/// Validate the complete snapshot/current-tree manifest before allowing the
/// caller to apply even the first ACL. The callback boundary makes partial
/// restore impossible for malformed or incomplete plans.
#[cfg(any(windows, test))]
fn run_validated_acl_restore_plan(
    snapshot: &[AclSnapshotPathFact],
    current: &[AclCurrentPathFact],
    mut apply: impl FnMut(usize) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    ensure!(!snapshot.is_empty(), "ACL snapshot is empty");
    ensure!(
        snapshot.len() <= MAX_ACL_SNAPSHOT_ENTRIES,
        "ACL snapshot exceeds the {MAX_ACL_SNAPSHOT_ENTRIES}-entry hard limit"
    );
    ensure!(
        current.len() <= MAX_MAINTENANCE_TREE_NODES,
        "current managed tree exceeds the {MAX_MAINTENANCE_TREE_NODES}-node hard limit"
    );

    let mut snapshot_order = Vec::new();
    try_reserve_bounded(
        &mut snapshot_order,
        snapshot.len(),
        MAX_ACL_SNAPSHOT_ENTRIES,
        "ACL snapshot validation index",
    )?;
    for (index, entry) in snapshot.iter().enumerate() {
        ensure!(
            entry.valid_relative_path,
            "ACL snapshot contains a non-relative managed path"
        );
        snapshot_order.push(index);
    }
    snapshot_order
        .sort_unstable_by(|left, right| snapshot[*left].path_key.cmp(&snapshot[*right].path_key));
    ensure!(
        snapshot_order
            .windows(2)
            .all(|pair| snapshot[pair[0]].path_key != snapshot[pair[1]].path_key),
        "ACL snapshot contains a duplicate path"
    );
    ensure!(
        snapshot_order
            .iter()
            .any(|index| snapshot[*index].path_key.is_empty()),
        "ACL snapshot does not contain its managed root"
    );

    let mut current_order = Vec::new();
    try_reserve_bounded(
        &mut current_order,
        current.len(),
        MAX_MAINTENANCE_TREE_NODES,
        "current managed tree validation index",
    )?;
    for (index, entry) in current.iter().enumerate() {
        ensure!(
            !entry.is_reparse_point,
            "current managed tree contains a reparse point"
        );
        ensure!(
            entry.is_directory ^ entry.is_regular_file,
            "current managed tree contains a special filesystem object"
        );
        if entry.is_regular_file {
            ensure!(
                entry.hard_link_count == Some(1),
                "current managed tree contains a multiply linked file"
            );
        }
        current_order.push(index);
    }
    current_order
        .sort_unstable_by(|left, right| current[*left].path_key.cmp(&current[*right].path_key));
    ensure!(
        current_order
            .windows(2)
            .all(|pair| current[pair[0]].path_key != current[pair[1]].path_key),
        "current managed tree contains a duplicate path"
    );

    ensure!(
        snapshot_order.len() == current_order.len()
            && snapshot_order
                .iter()
                .zip(&current_order)
                .all(
                    |(snapshot_index, current_index)| snapshot[*snapshot_index].path_key
                        == current[*current_index].path_key
                ),
        "ACL snapshot path set does not exactly match the current managed tree"
    );
    for (snapshot_index, current_index) in snapshot_order.iter().zip(&current_order) {
        ensure!(
            snapshot[*snapshot_index].is_directory == current[*current_index].is_directory,
            "ACL snapshot target changed type"
        );
    }

    snapshot_order.sort_by_key(|index| snapshot[*index].depth);
    for index in snapshot_order {
        apply(index)?;
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn program_security_descriptor(service_sid: &str) -> String {
    format!(
        "O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)\
         (A;OICI;0x1200a9;;;BU)(A;OICI;0x1200a9;;;{service_sid})"
    )
}

#[cfg(any(windows, test))]
fn managed_state_security_descriptor(service_sid: Option<&str>) -> String {
    let service_ace = service_sid
        .map(|sid| format!("(A;OICI;0x1301bf;;;{sid})"))
        .unwrap_or_default();
    format!("O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA){service_ace}(A;OICI;RC;;;OW)")
}

#[cfg(any(windows, test))]
fn managed_security_descriptor_for_target(
    descriptor: &str,
    is_directory: bool,
) -> std::borrow::Cow<'_, str> {
    if is_directory {
        std::borrow::Cow::Borrowed(descriptor)
    } else {
        std::borrow::Cow::Owned(descriptor.replace(";OICI;", ";;"))
    }
}

#[cfg(any(windows, test))]
fn protected_directory_security_descriptor() -> String {
    managed_state_security_descriptor(None)
}

#[cfg(any(windows, test))]
fn is_protected_dacl_control(control: &str) -> bool {
    // Windows can retain SE_DACL_AUTO_INHERITED while setting SE_DACL_PROTECTED
    // on an object that previously inherited its ACL. SDDL renders that inert
    // retained marker as PAI; P still prevents any future inheritance. Do
    // not accept AR, AI without P, or unknown flags.
    matches!(control, "P" | "PAI")
}

#[cfg(any(windows, test))]
fn has_exact_ace_inheritance_flags(flags: &str, is_directory: bool) -> bool {
    if is_directory {
        matches!(flags, "OICI" | "CIOI")
    } else {
        flags.is_empty()
    }
}

#[cfg(any(windows, test))]
fn parse_program_dacl(sddl: &str, service_sid: &str, is_directory: bool) -> anyhow::Result<()> {
    ensure!(
        sddl.starts_with("O:SY"),
        "program owner is not SYSTEM: {sddl}"
    );
    let dacl = sddl
        .split_once("D:")
        .map(|(_, value)| value)
        .context("program security descriptor has no DACL")?;
    let (control, _) = dacl
        .split_once('(')
        .context("program DACL contains no ACEs")?;
    ensure!(
        is_protected_dacl_control(control),
        "program DACL has unexpected protection flags: {sddl}"
    );
    let mut system = false;
    let mut admins = false;
    let mut users = false;
    let mut service = false;
    for ace in dacl.split('(').skip(1) {
        let ace = ace
            .split(')')
            .next()
            .context("malformed program DACL ACE")?;
        let fields = ace.split(';').collect::<Vec<_>>();
        ensure!(
            fields.len() == 6
                && fields[0] == "A"
                && has_exact_ace_inheritance_flags(fields[1], is_directory)
                && fields[3].is_empty()
                && fields[4].is_empty(),
            "unexpected program DACL ACE: ({ace})"
        );
        match fields[5] {
            "SY" | "S-1-5-18" => {
                ensure!(!system, "duplicate program SYSTEM ACE");
                ensure!(fields[2] == "FA", "program SYSTEM ACE is not full access");
                system = true;
            }
            "BA" | "S-1-5-32-544" => {
                ensure!(!admins, "duplicate program Administrators ACE");
                ensure!(
                    fields[2] == "FA",
                    "program Administrators ACE is not full access"
                );
                admins = true;
            }
            "BU" | "S-1-5-32-545" => {
                ensure!(!users, "duplicate program BUILTIN\\Users ACE");
                ensure!(
                    matches!(fields[2], "0x1200a9" | "0x001200a9"),
                    "program BUILTIN\\Users ACE is not exactly read/execute"
                );
                users = true;
            }
            trustee if trustee == service_sid => {
                ensure!(!service, "duplicate program service SID ACE");
                ensure!(
                    matches!(fields[2], "0x1200a9" | "0x001200a9"),
                    "program service ACE is not exactly read/execute"
                );
                service = true;
            }
            trustee => bail!("unexpected program DACL trustee {trustee}"),
        }
    }
    ensure!(
        system && admins && users && service,
        "program DACL does not match the current SYSTEM, Administrators, Users and service SID template"
    );
    Ok(())
}

#[cfg(any(windows, test))]
fn parse_managed_dacl(
    sddl: &str,
    service_sid: &str,
    require_service_access: bool,
    is_directory: bool,
) -> anyhow::Result<()> {
    ensure!(
        sddl.starts_with("O:SY"),
        "managed state owner is not SYSTEM: {sddl}"
    );
    let dacl = sddl
        .split_once("D:")
        .map(|(_, value)| value)
        .context("security descriptor has no DACL")?;
    let (control, _) = dacl
        .split_once('(')
        .context("managed state DACL contains no ACEs")?;
    ensure!(
        is_protected_dacl_control(control),
        "managed state DACL has unexpected protection flags: {sddl}"
    );
    let mut system = false;
    let mut admins = false;
    let mut owner_rights = false;
    let mut service = false;
    for ace in dacl.split('(').skip(1) {
        let ace = ace.split(')').next().context("malformed DACL ACE")?;
        let fields = ace.split(';').collect::<Vec<_>>();
        ensure!(
            fields.len() == 6 && fields[0] == "A",
            "unexpected DACL ACE: ({ace})"
        );
        ensure!(
            has_exact_ace_inheritance_flags(fields[1], is_directory),
            "managed state ACE flags do not match the target type: ({ace})"
        );
        ensure!(
            fields[3].is_empty() && fields[4].is_empty(),
            "managed state contains an object-specific ACE: ({ace})"
        );
        match fields[5] {
            "SY" | "S-1-5-18" => {
                ensure!(!system, "managed state contains duplicate SYSTEM ACEs");
                ensure!(
                    fields[2] == "FA",
                    "SYSTEM does not have exactly full access"
                );
                system = true;
            }
            "BA" | "S-1-5-32-544" => {
                ensure!(
                    !admins,
                    "managed state contains duplicate Administrators ACEs"
                );
                ensure!(
                    fields[2] == "FA",
                    "Administrators do not have exactly full access"
                );
                admins = true;
            }
            "OW" | "S-1-3-4" => {
                ensure!(
                    !owner_rights,
                    "managed state contains duplicate OWNER RIGHTS ACEs"
                );
                ensure!(
                    fields[2] == "RC",
                    "OWNER RIGHTS does not have ReadPermissions only"
                );
                owner_rights = true;
            }
            trustee if service_sid == trustee => {
                ensure!(
                    !service,
                    "managed state contains duplicate service SID ACEs"
                );
                ensure!(
                    matches!(fields[2], "0x1301bf" | "0x001301bf"),
                    "service SID does not have exactly Modify access"
                );
                service = true;
            }
            trustee => bail!("unexpected state DACL trustee {trustee}"),
        }
    }
    ensure!(
        system && admins && owner_rights,
        "managed state DACL is incomplete"
    );
    ensure!(
        !require_service_access || service,
        "managed state DACL lacks the service SID"
    );
    ensure!(
        require_service_access || !service,
        "preserved state still grants the service SID"
    );
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn entry() {
    eprintln!("host-monitor-maintenance is available only on Windows");
    std::process::exit(2);
}

#[cfg(windows)]
pub(crate) fn entry() {
    if let Err(error) = windows_maintenance::run() {
        eprintln!("host-monitor maintenance failed: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod windows_maintenance {
    use super::{
        AclCurrentPathFact, AclSnapshotPathFact, MAINTENANCE_DIAGNOSTIC_MAX_BYTES,
        MAINTENANCE_DIAGNOSTIC_SDDL, MAX_ACL_SNAPSHOT_BYTES, MAX_ACL_SNAPSHOT_ENTRIES,
        MAX_MAINTENANCE_PATH_BYTES, MAX_MAINTENANCE_TREE_NODES, MAX_OPEN_MUTATION_DIRECTORIES,
        OpenedManagedTargetFacts, checked_child_directory_depth, checked_rename_buffer_plan,
        checked_rename_storage_bytes, enqueue_bounded_tree_path, maintenance_diagnostic_payload,
        managed_security_descriptor_for_target, managed_state_security_descriptor,
        parse_maintenance_arguments, parse_managed_dacl, parse_program_dacl,
        program_security_descriptor, protected_directory_security_descriptor, read_file_bounded,
        record_bounded_tree_node, rollback_path_exists, run_validated_acl_restore_plan,
        try_push_bounded_acl_snapshot_entry, try_push_bounded_path, try_reserve_bounded,
        validate_opened_managed_target_facts,
    };
    use std::{
        ffi::{OsStr, OsString, c_void},
        fs,
        mem::size_of,
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        },
        path::{Component, Path, PathBuf},
        ptr, thread,
        time::{Duration, Instant},
    };

    use anyhow::{Context, bail, ensure};
    use host_monitor::{ClientConfig, service::WINDOWS_SERVICE_NAME};
    use serde::{Deserialize, Serialize};
    use windows::{
        Win32::{
            Foundation::{
                CloseHandle, ERROR_NOT_ALL_ASSIGNED, ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SUCCESS,
                GENERIC_WRITE, GetLastError, HANDLE, HLOCAL, LocalFree, SetLastError,
            },
            Security::{
                AdjustTokenPrivileges,
                Authorization::{
                    ConvertSecurityDescriptorToStringSecurityDescriptorW,
                    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
                    SDDL_REVISION_1, SE_FILE_OBJECT,
                },
                DACL_SECURITY_INFORMATION, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
                OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
                PSECURITY_DESCRIPTOR, SE_PRIVILEGE_ENABLED, SE_RESTORE_NAME,
                SE_TAKE_OWNERSHIP_NAME, SECURITY_ATTRIBUTES, SetFileSecurityW,
                TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
                UNPROTECTED_DACL_SECURITY_INFORMATION,
            },
            Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE,
                FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
                FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
                FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_MODE, FILE_SHARE_READ,
                FILE_SHARE_WRITE, FILE_TRAVERSE, FileDispositionInfo, FileRenameInfo,
                FlushFileBuffers, GetFileInformationByHandle, OPEN_EXISTING, READ_CONTROL,
                SetFileInformationByHandle, WRITE_DAC, WRITE_OWNER, WriteFile,
            },
            System::{
                Com::CoTaskMemFree,
                Services::{
                    ChangeServiceConfig2W, CloseServiceHandle, ControlService, OpenSCManagerW,
                    OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfig2W, QueryServiceConfigW,
                    QueryServiceStatusEx, SC_HANDLE, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO,
                    SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
                    SERVICE_CONFIG_SERVICE_SID_INFO, SERVICE_CONTROL_STOP, SERVICE_DEMAND_START,
                    SERVICE_DISABLED, SERVICE_FAILURE_ACTIONS_FLAG, SERVICE_QUERY_CONFIG,
                    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_SID_INFO,
                    SERVICE_SID_TYPE_UNRESTRICTED, SERVICE_START, SERVICE_START_PENDING,
                    SERVICE_STATUS, SERVICE_STATUS_PROCESS, SERVICE_STOP, SERVICE_STOP_PENDING,
                    SERVICE_STOPPED, SERVICE_WIN32_OWN_PROCESS, StartServiceW,
                },
                Threading::{GetCurrentProcess, OpenProcessToken},
            },
            UI::Shell::{
                FOLDERID_ProgramData, FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
            },
        },
        core::{PCWSTR, PWSTR},
    };

    const DIRECTORY_NAME: &str = "host-monitor";
    const CLIENT_EXE: &str = "host-monitor.exe";
    const CONFIG_FILE: &str = "config.json";
    const JOURNAL_DIRECTORY: &str =
        concat!("host-monitor.install-journal-", env!("CARGO_PKG_VERSION"));
    const UNINSTALL_JOURNAL_DIRECTORY: &str =
        concat!("host-monitor.uninstall-journal-", env!("CARGO_PKG_VERSION"));
    const PURGE_DIRECTORY: &str =
        concat!("host-monitor.purge-quarantine-", env!("CARGO_PKG_VERSION"));
    const DIAGNOSTIC_FILE: &str = concat!(
        "host-monitor.maintenance-diagnostic-",
        env!("CARGO_PKG_VERSION"),
        ".txt"
    );
    const SNAPSHOT_FORMAT: u32 = 2;
    const STATE_MARKER: &str = concat!(".host-monitor-managed-", env!("CARGO_PKG_VERSION"));
    const STATE_MARKER_CONTENT: &str = concat!(
        "host-monitor-windows-state-",
        env!("CARGO_PKG_VERSION"),
        "\r\n"
    );
    const SNAPSHOT_FILE: &str = "snapshot.json";
    const STATE_ACL_FILE: &str = "state-acl.json";
    const PROGRAM_ACL_FILE: &str = "program-acl.json";
    const PURGE_STARTED_FILE: &str = concat!("purge-started-v2-", env!("CARGO_PKG_VERSION"));
    const PURGE_STARTED_CONTENT: &str = concat!(
        "host-monitor-purge-started-v2-",
        env!("CARGO_PKG_VERSION"),
        "\r\n"
    );
    const STOP_TIMEOUT: Duration = Duration::from_secs(30);

    #[derive(Debug)]
    struct FixedPaths {
        program_root: PathBuf,
        state_root: PathBuf,
        journal_root: PathBuf,
        uninstall_journal_root: PathBuf,
        quarantine_root: PathBuf,
        diagnostic_file: PathBuf,
        program_exe: PathBuf,
        config: PathBuf,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct InstallSnapshot {
        format: u32,
        application_version: String,
        program_existed: bool,
        state_existed: bool,
        original_service_sid_type: Option<u32>,
        original_failure_actions_on_non_crash: Option<bool>,
        original_service_running: bool,
        state_acl_saved: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct UninstallSnapshot {
        format: u32,
        application_version: String,
        state_acl_saved: bool,
        service_was_running: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AclSnapshot {
        format: u32,
        application_version: String,
        entries: Vec<AclSnapshotEntry>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AclSnapshotEntry {
        relative_path_utf16: Vec<u16>,
        is_directory: bool,
        sddl: String,
    }

    pub fn run() -> anyhow::Result<()> {
        let invocation = parse_maintenance_arguments(std::env::args_os().skip(1))?;
        let paths = FixedPaths::discover()?;
        let result = (|| {
            enable_restore_privileges()?;
            match invocation.command.as_str() {
                "prepare-install" => prepare_install(&paths),
                "apply-install" => apply_install(&paths),
                "rollback-install" => rollback_install(&paths),
                "commit-install" => commit_install(&paths),
                "preflight-uninstall" => preflight_uninstall(&paths),
                "rollback-uninstall-preflight" => rollback_uninstall_preflight(&paths),
                "preserve-state" => preserve_state(&paths),
                "rollback-uninstall" => rollback_uninstall(&paths),
                "commit-uninstall" => commit_uninstall(&paths),
                "prepare-purge" => prepare_purge(&paths),
                "rollback-purge" => rollback_purge(&paths),
                "commit-purge" => commit_purge(&paths),
                _ => bail!(
                    "unknown maintenance command; expected prepare-install, apply-install, \
                 rollback-install, commit-install, preflight-uninstall, preserve-state, \
                 rollback-uninstall-preflight, rollback-uninstall, commit-uninstall, \
                 prepare-purge, rollback-purge, or commit-purge"
                ),
            }
        })();
        if invocation.diagnostics
            && let Err(error) = &result
        {
            let _ = write_first_maintenance_diagnostic(
                &paths.diagnostic_file,
                &invocation.command,
                error,
            );
        }
        result
    }

    fn enable_restore_privileges() -> anyhow::Result<()> {
        let mut token = HANDLE::default();
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            )
        }
        .context("failed to open the maintenance process token")?;
        let result = (|| {
            for (name, label) in [
                (SE_RESTORE_NAME, "SeRestorePrivilege"),
                (SE_TAKE_OWNERSHIP_NAME, "SeTakeOwnershipPrivilege"),
            ] {
                let mut luid = Default::default();
                unsafe { LookupPrivilegeValueW(None, name, &mut luid) }
                    .with_context(|| format!("failed to resolve {label}"))?;
                let privileges = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES {
                        Luid: luid,
                        Attributes: SE_PRIVILEGE_ENABLED,
                    }],
                };
                unsafe { SetLastError(ERROR_SUCCESS) };
                unsafe { AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None) }
                    .with_context(|| format!("failed to enable {label}"))?;
                ensure!(
                    unsafe { GetLastError() } != ERROR_NOT_ALL_ASSIGNED,
                    "maintenance token does not hold {label}"
                );
            }
            Ok(())
        })();
        let close = unsafe { CloseHandle(token) }.context("failed to close process token");
        result.and(close)
    }

    impl FixedPaths {
        fn discover() -> anyhow::Result<Self> {
            let program_files = known_folder(&FOLDERID_ProgramFiles)?;
            let program_data = known_folder(&FOLDERID_ProgramData)?;
            ensure_absolute_root(&program_files, "Program Files")?;
            ensure_absolute_root(&program_data, "ProgramData")?;
            let program_root = program_files.join(DIRECTORY_NAME);
            let state_root = program_data.join(DIRECTORY_NAME);
            Ok(Self {
                program_exe: program_root.join(CLIENT_EXE),
                config: state_root.join(CONFIG_FILE),
                journal_root: program_data.join(JOURNAL_DIRECTORY),
                uninstall_journal_root: program_data.join(UNINSTALL_JOURNAL_DIRECTORY),
                quarantine_root: program_data.join(PURGE_DIRECTORY),
                diagnostic_file: program_data.join(DIAGNOSTIC_FILE),
                program_root,
                state_root,
            })
        }
    }

    fn known_folder(id: &windows::core::GUID) -> anyhow::Result<PathBuf> {
        let value = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }
            .context("SHGetKnownFolderPath failed")?;
        let text = unsafe { value.to_string() }.context("known folder path is invalid")?;
        unsafe { CoTaskMemFree(Some(value.0.cast())) };
        Ok(PathBuf::from(text))
    }

    fn ensure_absolute_root(path: &Path, label: &str) -> anyhow::Result<()> {
        ensure!(
            path.is_absolute(),
            "{label} did not resolve to an absolute path"
        );
        ensure!(
            path.parent().is_some(),
            "{label} resolved to a filesystem root"
        );
        Ok(())
    }

    // Keep the installer transaction as one private module while splitting its
    // implementation by responsibility. This avoids widening access to the
    // rollback journal and fixed-path invariants merely to shorten this file.
    include!("transaction.rs");

    include!("filesystem.rs");

    include!("acl.rs");

    include!("service.rs");
    include!("tests.rs");
}

#[cfg(test)]
mod program_acl_template_tests {
    use super::*;

    const SERVICE_SID: &str = "S-1-5-80-1-2-3-4-5";

    #[test]
    fn parser_accepts_only_the_current_program_template() {
        let incomplete_service_only = format!(
            "O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)\
             (A;OICI;0x1200a9;;;{SERVICE_SID})"
        );
        assert!(parse_program_dacl(&incomplete_service_only, SERVICE_SID, true).is_err());

        let directory = program_security_descriptor(SERVICE_SID);
        let file = managed_security_descriptor_for_target(&directory, false);
        assert_eq!(
            managed_security_descriptor_for_target(&directory, true),
            directory
        );
        assert!(!file.contains("OICI"));
        parse_program_dacl(&directory, SERVICE_SID, true).unwrap();
        parse_program_dacl(&file, SERVICE_SID, false).unwrap();
        parse_program_dacl(&directory.replacen("D:P", "D:PAI", 1), SERVICE_SID, true).unwrap();
        parse_program_dacl(&file.replacen("D:P", "D:PAI", 1), SERVICE_SID, false).unwrap();
        assert!(parse_program_dacl(&directory, SERVICE_SID, false).is_err());
        assert!(parse_program_dacl(&file, SERVICE_SID, true).is_err());

        for unsupported_control in ["", "AI", "PAR", "PAIAR", "PX"] {
            let unsupported = directory.replacen("D:P", &format!("D:{unsupported_control}"), 1);
            assert!(parse_program_dacl(&unsupported, SERVICE_SID, true).is_err());
        }

        let users_can_write = directory.replace("(A;OICI;0x1200a9;;;BU)", "(A;OICI;FA;;;BU)");
        assert!(parse_program_dacl(&users_can_write, SERVICE_SID, true).is_err());

        let unexpected_authenticated_users =
            directory.replace("(A;OICI;0x1200a9;;;BU)", "(A;OICI;0x1200a9;;;AU)");
        assert!(parse_program_dacl(&unexpected_authenticated_users, SERVICE_SID, true).is_err());
    }

    #[test]
    fn managed_state_parser_matches_directory_and_file_ace_flags() {
        let installed_directory = managed_state_security_descriptor(Some(SERVICE_SID));
        let installed_file = installed_directory.replace(";OICI;", ";;");
        parse_managed_dacl(&installed_directory, SERVICE_SID, true, true).unwrap();
        parse_managed_dacl(&installed_file, SERVICE_SID, true, false).unwrap();
        parse_managed_dacl(
            &installed_file.replacen("D:P", "D:PAI", 1),
            SERVICE_SID,
            true,
            false,
        )
        .unwrap();
        assert!(parse_managed_dacl(&installed_directory, SERVICE_SID, true, false).is_err());
        assert!(parse_managed_dacl(&installed_file, SERVICE_SID, true, true).is_err());

        let preserved_directory = managed_state_security_descriptor(None);
        let preserved_file = managed_security_descriptor_for_target(&preserved_directory, false);
        parse_managed_dacl(&preserved_directory, SERVICE_SID, false, true).unwrap();
        parse_managed_dacl(&preserved_file, SERVICE_SID, false, false).unwrap();
    }

    #[test]
    fn tray_execute_access_never_leaks_into_mutable_state_template() {
        let program = program_security_descriptor(SERVICE_SID);
        assert!(program.contains("(A;OICI;0x1200a9;;;BU)"));

        let installed_state = managed_state_security_descriptor(Some(SERVICE_SID));
        let preserved_state = managed_state_security_descriptor(None);
        assert!(!installed_state.contains(";;;BU)"));
        assert!(!preserved_state.contains(";;;BU)"));
        assert_eq!(
            installed_state,
            format!(
                "O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)\
                 (A;OICI;0x1301bf;;;{SERVICE_SID})(A;OICI;RC;;;OW)"
            )
        );
    }

    #[test]
    fn protected_directory_creation_contract_is_atomic_and_system_admin_only() {
        assert_eq!(
            protected_directory_security_descriptor(),
            "O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;RC;;;OW)"
        );

        let filesystem = include_str!("filesystem.rs");
        let transaction = include_str!("transaction.rs");
        assert!(filesystem.contains("CreateDirectoryW("));
        assert!(filesystem.contains("SECURITY_ATTRIBUTES {"));
        assert!(transaction.contains("create_system_admin_only_directory(&paths.state_root"));
        assert!(!filesystem.contains("fs::create_dir("));
        assert!(!filesystem.contains("fs::create_dir_all("));
        assert!(!transaction.contains("fs::create_dir("));
        assert!(!transaction.contains("fs::create_dir_all("));
    }
}

#[cfg(test)]
mod rollback_path_tests {
    use super::*;

    #[test]
    fn metadata_errors_are_not_treated_as_missing_rollback_paths() {
        let path = std::path::Path::new("protected-journal");
        assert!(!checked_rollback_path_status(path, "install journal", Ok(false)).unwrap());

        let error = checked_rollback_path_status(
            path,
            "install journal",
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "simulated metadata denial",
            )),
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("failed to inspect install journal"));
        assert!(message.contains("simulated metadata denial"));
    }
}

#[cfg(test)]
mod managed_handle_target_tests {
    use super::*;

    #[test]
    fn opened_target_must_keep_its_safe_file_identity() {
        validate_opened_managed_target_facts(OpenedManagedTargetFacts {
            expected_directory: false,
            actual_directory: false,
            is_reparse_point: false,
            hard_link_count: 1,
        })
        .unwrap();
        validate_opened_managed_target_facts(OpenedManagedTargetFacts {
            expected_directory: true,
            actual_directory: true,
            is_reparse_point: false,
            hard_link_count: 3,
        })
        .unwrap();

        let reparse = validate_opened_managed_target_facts(OpenedManagedTargetFacts {
            expected_directory: false,
            actual_directory: false,
            is_reparse_point: true,
            hard_link_count: 1,
        })
        .unwrap_err();
        assert!(format!("{reparse:#}").contains("reparse point"));

        let hard_link = validate_opened_managed_target_facts(OpenedManagedTargetFacts {
            expected_directory: false,
            actual_directory: false,
            is_reparse_point: false,
            hard_link_count: 2,
        })
        .unwrap_err();
        assert!(format!("{hard_link:#}").contains("multiply linked file"));

        let changed_type = validate_opened_managed_target_facts(OpenedManagedTargetFacts {
            expected_directory: true,
            actual_directory: false,
            is_reparse_point: false,
            hard_link_count: 1,
        })
        .unwrap_err();
        assert!(format!("{changed_type:#}").contains("changed type"));

        let source = include_str!("acl.rs");
        assert!(source.contains("GetSecurityInfo("));
        assert!(source.contains("SetFileSecurityW("));
        assert!(
            source.contains("managed_security_descriptor_for_target(sddl, handle.is_directory)")
        );
        assert!(source.contains(
            "set_security_descriptor(path, sddl, saved_dacl_is_protected(sddl)?, false)"
        ));
        assert!(!source.contains("GetNamedSecurityInfoW("));
        assert!(!source.contains("SetSecurityInfo("));

        let opener = source
            .split_once("fn open_acl_target(")
            .unwrap()
            .1
            .split_once("fn open_acl_target_for_read(")
            .unwrap()
            .0;
        assert!(opener.contains("FILE_FLAG_OPEN_REPARSE_POINT"));
        assert!(opener.contains("GetFileInformationByHandle("));
        assert!(opener.contains("FILE_SHARE_READ | FILE_SHARE_WRITE,"));
        assert!(!opener.contains("FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE"));

        let setter = source.split_once("fn set_security_descriptor(").unwrap().1;
        let opened = setter
            .find("let handle = open_acl_target_for_write(path)?;")
            .unwrap();
        let applied = setter.find("SetFileSecurityW(").unwrap();
        let captured = setter.find(".ok()").unwrap();
        let released = setter.find("drop(handle);").unwrap();
        assert!(opened < applied && applied < captured && captured < released);
        assert_eq!(setter.matches("SetFileSecurityW(").count(), 1);
        assert!(setter.contains("OWNER_SECURITY_INFORMATION"));
        assert!(setter.contains("DACL_SECURITY_INFORMATION"));
        assert!(setter.contains("PROTECTED_DACL_SECURITY_INFORMATION"));
        assert!(setter.contains("UNPROTECTED_DACL_SECURITY_INFORMATION"));
        assert!(setter.contains("descriptor.0"));
    }

    #[test]
    fn recursive_acl_updates_protect_descendants_before_ancestors() {
        let source = include_str!("acl.rs");
        assert_eq!(
            source
                .matches("for target in targets.into_iter().rev() {")
                .count(),
            2,
            "both recursive ACL application paths must run child-first"
        );
        assert!(
            !source.contains("for target in targets {"),
            "recursive ACL updates must retain per-target child-first validation"
        );
    }

    #[test]
    fn rename_plan_is_aligned_bounded_and_used_for_all_tree_mutations() {
        assert_eq!(MAX_OPEN_MUTATION_DIRECTORIES, 256);
        assert_eq!(checked_child_directory_depth(255, 256).unwrap(), 256);
        assert!(checked_child_directory_depth(256, 256).is_err());
        assert!(checked_child_directory_depth(usize::MAX, usize::MAX).is_err());

        let plan = RenameBufferPlan {
            file_name_bytes: 8,
            buffer_bytes: 30,
            storage_words: 4,
        };
        assert_eq!(checked_rename_buffer_plan(4, 20, 8, 64).unwrap(), plan);
        assert_eq!(checked_rename_storage_bytes(plan, 8, 24, 20).unwrap(), 32);
        assert!(
            checked_rename_storage_bytes(
                RenameBufferPlan {
                    storage_words: 2,
                    ..plan
                },
                8,
                24,
                20,
            )
            .is_err()
        );
        assert!(checked_rename_buffer_plan(0, 20, 8, 64).is_err());
        assert!(checked_rename_buffer_plan(4, 20, 8, 29).is_err());
        assert!(checked_rename_buffer_plan(usize::MAX, 20, 8, usize::MAX).is_err());

        let filesystem = include_str!("filesystem.rs");
        let transaction = include_str!("transaction.rs");
        let removal = filesystem
            .split_once("fn remove_tree_no_reparse")
            .unwrap()
            .1
            .split_once("fn rename_managed_directory_by_handle")
            .unwrap()
            .0;
        assert!(
            removal
                .find("validate_tree_with_directory_depth_limit")
                .unwrap()
                < removal.find("delete_opened_mutation_target").unwrap()
        );
        let empty_only = filesystem
            .split_once("fn remove_empty_directory_by_handle")
            .unwrap()
            .1
            .split_once("struct RemovalDirectoryFrame")
            .unwrap()
            .0;
        assert!(empty_only.contains("entries.next()"));
        assert!(empty_only.contains("delete_opened_mutation_target"));
        assert!(!empty_only.contains("remove_tree_no_reparse"));
        assert!(filesystem.contains("SetFileInformationByHandle("));
        assert!(filesystem.contains("FileDispositionInfo"));
        assert!(filesystem.contains("FileRenameInfo"));
        assert!(filesystem.contains("Vec::<usize>::new()"));
        assert!(filesystem.contains("validate_tree_with_directory_depth_limit("));
        assert!(filesystem.contains("managed tree root after handle-bound deletion"));
        assert!(filesystem.contains("managed rename source after handle-bound rename"));
        let rename = filesystem
            .split_once("fn rename_managed_directory_by_handle")
            .unwrap()
            .1
            .split_once("fn write_new_private")
            .unwrap()
            .0;
        assert!(rename.contains("destination.parent() == Some(source_parent)"));
        assert!(rename.contains(".file_name()"));
        assert!(rename.contains("source.is_absolute()"));
        assert!(rename.contains("Component::Normal(_)"));
        assert!(rename.contains("u16::from(b':')"));
        assert!(rename.contains("(*information).RootDirectory = HANDLE::default();"));
        assert!(!rename.contains("RootDirectory = parent_handle.0"));
        assert_eq!(
            rename
                .matches("destination.as_os_str().encode_wide()")
                .count(),
            2
        );
        let opened_parent = rename.find("open_rename_parent(source_parent)").unwrap();
        let checked_destination = rename
            .find("ensure_absent(destination, destination_label)")
            .unwrap();
        let opened_source = rename.find("open_mutation_target(source, true").unwrap();
        let applied = rename.find("SetFileInformationByHandle(").unwrap();
        let verified = rename
            .find("validate_real_directory(destination, destination_label)")
            .unwrap();
        let released_source = rename.find("drop(handle);").unwrap();
        let released_parent = rename.find("drop(parent_handle);").unwrap();
        assert!(
            opened_parent < opened_source
                && opened_parent < checked_destination
                && checked_destination < opened_source
                && opened_source < applied
                && applied < verified
                && verified < released_source
                && released_source < released_parent
        );
        let parent_opener = filesystem
            .split_once("fn open_rename_parent")
            .unwrap()
            .1
            .split_once("fn delete_opened_mutation_target")
            .unwrap()
            .0;
        assert!(parent_opener.contains("FILE_TRAVERSE.0 | FILE_READ_ATTRIBUTES.0"));
        assert!(!parent_opener.contains("DELETE.0"));
        assert!(transaction.contains("remove_empty_directory_by_handle("));
        assert!(!filesystem.contains("fs::rename("));
        assert!(!filesystem.contains("fs::remove_dir("));
        assert!(!transaction.contains("fs::rename("));
        assert!(!transaction.contains("fs::remove_dir("));
    }
}

#[cfg(test)]
mod acl_restore_plan_tests {
    use super::*;

    fn snapshot(path: &[u16], depth: usize, valid: bool) -> AclSnapshotPathFact {
        AclSnapshotPathFact {
            path_key: path.to_vec(),
            depth,
            valid_relative_path: valid,
            is_directory: path.is_empty(),
        }
    }

    fn current(path: &[u16]) -> AclCurrentPathFact {
        AclCurrentPathFact {
            path_key: path.to_vec(),
            is_directory: path.is_empty(),
            is_regular_file: !path.is_empty(),
            is_reparse_point: false,
            hard_link_count: (!path.is_empty()).then_some(1),
        }
    }

    #[test]
    fn incomplete_or_late_invalid_manifests_apply_nothing() {
        let root = snapshot(&[], 0, true);
        let child = snapshot(&[b'a' as u16], 1, true);
        let root_current = current(&[]);
        let child_current = current(&[b'a' as u16]);

        let mut applied = 0;
        let missing = run_validated_acl_restore_plan(
            std::slice::from_ref(&root),
            &[root_current.clone(), child_current.clone()],
            |_| {
                applied += 1;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(missing.to_string().contains("does not exactly match"));
        assert_eq!(applied, 0, "an incomplete snapshot must apply no ACLs");

        let late_invalid = snapshot(&[b'z' as u16], 1, false);
        let mut applied = 0;
        let invalid = run_validated_acl_restore_plan(
            &[root, child, late_invalid],
            &[root_current, child_current, current(&[b'z' as u16])],
            |_| {
                applied += 1;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(invalid.to_string().contains("non-relative"));
        assert_eq!(applied, 0, "a malformed later entry must apply no ACLs");
    }
}

#[cfg(test)]
mod maintenance_resource_limit_tests {
    use super::*;

    #[test]
    fn small_tree_limits_bound_total_nodes_and_live_vectors() {
        let mut pending = Vec::new();
        let mut discovered = 0;
        let mut path_bytes = 0;
        enqueue_bounded_tree_path(
            &mut pending,
            &mut discovered,
            1,
            1,
            &mut path_bytes,
            2,
            1024,
            "test traversal",
        )
        .unwrap();
        assert_eq!(pending.pop(), Some(1));
        enqueue_bounded_tree_path(
            &mut pending,
            &mut discovered,
            2,
            1,
            &mut path_bytes,
            2,
            1024,
            "test traversal",
        )
        .unwrap();
        assert_eq!(pending.pop(), Some(2));
        let accepted_path_bytes = path_bytes;
        let error = enqueue_bounded_tree_path(
            &mut pending,
            &mut discovered,
            3,
            1,
            &mut path_bytes,
            2,
            1024,
            "test traversal",
        )
        .unwrap_err();
        assert!(error.to_string().contains("hard limit of 2 nodes"));
        assert!(pending.is_empty(), "the rejected node must not be queued");
        assert_eq!(path_bytes, accepted_path_bytes);

        let mut recorded = 0;
        record_bounded_tree_node(&mut recorded, 1, "test tree").unwrap();
        let error = record_bounded_tree_node(&mut recorded, 1, "test tree").unwrap_err();
        assert!(error.to_string().contains("hard limit of 1 nodes"));
    }

    #[test]
    fn rejected_path_payload_does_not_push_or_change_accounting() {
        const { assert!(MAX_MAINTENANCE_PATH_BYTES >= MAINTENANCE_PATH_FIXED_BYTES + 2) };
        let hard_limit = MAINTENANCE_PATH_FIXED_BYTES + 2;
        let mut path_bytes = 0;
        let mut paths = Vec::new();
        try_push_bounded_path(
            &mut paths,
            "accepted",
            1,
            &mut path_bytes,
            2,
            hard_limit,
            "test paths",
        )
        .unwrap();
        let accepted_bytes = path_bytes;

        let error = try_push_bounded_path(
            &mut paths,
            "rejected",
            1,
            &mut path_bytes,
            2,
            hard_limit,
            "test paths",
        )
        .unwrap_err();
        assert!(error.to_string().contains("cumulative path hard limit"));
        assert_eq!(path_bytes, accepted_bytes);
        assert_eq!(paths, ["accepted"]);
    }

    #[test]
    fn rejected_snapshot_payload_does_not_grow_accounting_or_entries() {
        let hard_limit = ACL_SNAPSHOT_ENTRY_FIXED_BYTES + 4;
        let mut accounted = 0;
        let mut entries = Vec::new();

        try_push_bounded_acl_snapshot_entry(
            &mut entries,
            "accepted",
            1,
            2,
            &mut accounted,
            2,
            hard_limit,
            "test snapshot entries",
        )
        .unwrap();
        let accepted_bytes = accounted;

        let error = try_push_bounded_acl_snapshot_entry(
            &mut entries,
            "rejected",
            1,
            1,
            &mut accounted,
            2,
            hard_limit,
            "test snapshot entries",
        )
        .unwrap_err();
        assert!(error.to_string().contains("cumulative hard limit"));
        assert_eq!(accounted, accepted_bytes);
        assert_eq!(entries, ["accepted"]);
    }

    #[test]
    fn sparse_snapshot_is_rejected_after_only_limit_plus_one_bytes() {
        let path = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!(
                "host-monitoring-maintenance-sparse-snapshot-{}.json",
                uuid::Uuid::new_v4()
            ));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(u64::try_from(MAX_ACL_SNAPSHOT_BYTES + 1).unwrap())
            .unwrap();
        drop(file);

        let error = read_file_bounded(&path, 64, "test ACL snapshot").unwrap_err();
        std::fs::remove_file(path).unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("64-byte hard limit"));
    }
}

#[cfg(test)]
mod maintenance_diagnostic_tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<std::ffi::OsString> {
        values.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn diagnostics_flag_is_optional_and_exact() {
        assert_eq!(
            parse_maintenance_arguments(arguments(&["apply-install"])).unwrap(),
            MaintenanceInvocation {
                command: "apply-install".to_owned(),
                diagnostics: false,
            }
        );
        assert_eq!(
            parse_maintenance_arguments(arguments(&["apply-install", "1"])).unwrap(),
            MaintenanceInvocation {
                command: "apply-install".to_owned(),
                diagnostics: true,
            }
        );
        for invalid in ["", "0", "01", "true", "１"] {
            assert!(parse_maintenance_arguments(arguments(&["apply-install", invalid])).is_err());
        }
        assert!(parse_maintenance_arguments(arguments(&["apply-install", "1", "extra"])).is_err());
        assert!(parse_maintenance_arguments(arguments(&["not-a-command", "1"])).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn command_and_diagnostics_flag_must_be_unicode() {
        use std::os::unix::ffi::OsStringExt;

        assert!(
            parse_maintenance_arguments(vec![std::ffi::OsString::from_vec(vec![0xff])]).is_err()
        );
        assert!(
            parse_maintenance_arguments(vec![
                std::ffi::OsString::from("apply-install"),
                std::ffi::OsString::from_vec(vec![0xff]),
            ])
            .is_err()
        );
    }

    #[test]
    fn diagnostic_payload_is_utf8_versioned_bounded_and_keeps_the_error_chain() {
        let error = anyhow::anyhow!("leaf failure").context("outer operation");
        let payload = maintenance_diagnostic_payload("apply-install", &error);
        let text = std::str::from_utf8(&payload).unwrap();
        assert!(text.contains("format=host-monitor-maintenance-diagnostic-v1\n"));
        assert!(text.contains(concat!("version=", env!("CARGO_PKG_VERSION"), "\n")));
        assert!(text.contains("command=apply-install\n"));
        assert!(text.contains("error-chain=outer operation: leaf failure\n"));

        let oversized = anyhow::anyhow!("critical leaf code 123")
            .context("诊断".repeat(MAINTENANCE_DIAGNOSTIC_MAX_BYTES));
        let bounded = maintenance_diagnostic_payload("apply-install", &oversized);
        assert!(bounded.len() <= MAINTENANCE_DIAGNOSTIC_MAX_BYTES);
        let bounded = std::str::from_utf8(&bounded).unwrap();
        assert!(bounded.contains("error-chain=[truncated]\n"));
        assert!(bounded.contains("truncated=true\n"));
        assert!(bounded.contains("leaf-cause=critical leaf code 123\n"));

        let oversized_leaf = anyhow::anyhow!("诊断".repeat(MAINTENANCE_DIAGNOSTIC_MAX_BYTES));
        let bounded_leaf = maintenance_diagnostic_payload("apply-install", &oversized_leaf);
        assert!(bounded_leaf.len() <= MAINTENANCE_DIAGNOSTIC_MAX_BYTES);
        assert_eq!(bounded_leaf.last(), Some(&b'\n'));
        let bounded_leaf = std::str::from_utf8(&bounded_leaf).unwrap();
        assert!(bounded_leaf.contains("error-chain=[truncated]\n"));
        assert!(bounded_leaf.contains("truncated=true\n"));
        assert!(bounded_leaf.contains("leaf-cause=诊断"));
    }

    #[test]
    fn diagnostic_file_creation_contract_is_fixed_secure_first_only_and_best_effort() {
        assert_eq!(
            MAINTENANCE_DIAGNOSTIC_SDDL,
            "O:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)"
        );
        let module = include_str!("mod.rs");
        let filesystem = include_str!("filesystem.rs");
        assert!(module.contains("program_data.join(DIAGNOSTIC_FILE)"));
        assert!(module.contains("if invocation.diagnostics && let Err(error) = &result"));
        assert!(module.contains("let _ = write_first_maintenance_diagnostic("));
        assert!(filesystem.contains("CreateFileW("));
        assert!(filesystem.contains("CREATE_NEW"));
        assert!(filesystem.contains("Some(&attributes)"));
        assert!(filesystem.contains("GENERIC_WRITE.0 | DELETE.0"));
        assert!(filesystem.contains("cleanup_required: true"));
        assert!(filesystem.contains("FlushFileBuffers(handle.handle)"));
        assert!(filesystem.contains("rename_diagnostic_to_final(handle.handle, path)?"));
        assert!(filesystem.contains("handle.cleanup_required = false"));
    }
}
