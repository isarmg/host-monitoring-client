fn secure_system_admin_only(path: &Path, recursive: bool) -> anyhow::Result<()> {
    apply_exact_acl(path, None, recursive)
}

fn secure_state_for_service(path: &Path) -> anyhow::Result<()> {
    let service_sid = service_sid_string()?;
    apply_exact_acl(path, Some(&service_sid), true)
}

fn secure_program_for_service(path: &Path) -> anyhow::Result<()> {
    let service_sid = service_sid_string()?;
    // The service keeps its isolated read/execute grant while BUILTIN\Users
    // receives the same non-writing rights needed to start the per-user tray
    // companion from HKLM\...\Run. Mutable state remains under the separate
    // `apply_exact_acl` template and deliberately never receives this ACE.
    let descriptor = program_security_descriptor(&service_sid);
    apply_descriptor_recursively(path, &descriptor)
}

fn validate_program_tree(path: &Path) -> anyhow::Result<()> {
    validate_tree(path)?;
    let mut pending = Vec::new();
    let mut discovered = 0;
    let mut path_payload_bytes = 0;
    enqueue_bounded_tree_path(
        &mut pending,
        &mut discovered,
        path.to_path_buf(),
        maintenance_path_utf16_units(path),
        &mut path_payload_bytes,
        MAX_MAINTENANCE_TREE_NODES,
        MAX_MAINTENANCE_PATH_BYTES,
        "program ACL validation traversal",
    )?;
    while let Some(target) = pending.pop() {
        validate_program_dacl(&target)?;
        if fs::symlink_metadata(&target)?.is_dir() {
            for child in fs::read_dir(&target)? {
                let child = child?.path();
                let path_utf16_units = maintenance_path_utf16_units(&child);
                enqueue_bounded_tree_path(
                    &mut pending,
                    &mut discovered,
                    child,
                    path_utf16_units,
                    &mut path_payload_bytes,
                    MAX_MAINTENANCE_TREE_NODES,
                    MAX_MAINTENANCE_PATH_BYTES,
                    "program ACL validation traversal",
                )?;
            }
        }
    }
    Ok(())
}

fn validate_program_dacl(path: &Path) -> anyhow::Result<()> {
    let (sddl, is_directory) = security_descriptor_sddl_with_target_type(path)?;
    parse_program_dacl(&sddl, &service_sid_string()?, is_directory)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProgramAclRestore {
    /// An install rollback has already restored the exact protected ACL
    /// snapshot from the current installation transaction.
    PreserveSnapshot,
    /// Uninstall rollback restores the currently installed tray-aware
    /// product, so rebuild and verify the current exact template.
    SecureCurrent,
}

fn apply_descriptor_recursively(path: &Path, descriptor: &str) -> anyhow::Result<()> {
    validate_tree(path)?;
    let mut targets = Vec::new();
    let mut path_payload_bytes = 0;
    try_push_bounded_path(
        &mut targets,
        path.to_path_buf(),
        maintenance_path_utf16_units(path),
        &mut path_payload_bytes,
        MAX_MAINTENANCE_TREE_NODES,
        MAX_MAINTENANCE_PATH_BYTES,
        "recursive ACL application targets",
    )?;
    let mut index = 0;
    while index < targets.len() {
        let current = targets[index].clone();
        if fs::symlink_metadata(&current)?.is_dir() {
            for entry in fs::read_dir(&current)? {
                let target = entry?.path();
                let path_utf16_units = maintenance_path_utf16_units(&target);
                try_push_bounded_path(
                    &mut targets,
                    target,
                    path_utf16_units,
                    &mut path_payload_bytes,
                    MAX_MAINTENANCE_TREE_NODES,
                    MAX_MAINTENANCE_PATH_BYTES,
                    "recursive ACL application targets",
                )?;
            }
        }
        index += 1;
    }
    // Apply every target explicitly and child-first. This avoids relying on
    // directory ACL propagation and preserves per-target handle validation.
    for target in targets.into_iter().rev() {
        set_managed_security_descriptor(&target, descriptor)?;
    }
    Ok(())
}

fn apply_exact_acl(path: &Path, service_sid: Option<&str>, recursive: bool) -> anyhow::Result<()> {
    let descriptor = managed_state_security_descriptor(service_sid);
    let mut targets = Vec::new();
    let mut path_payload_bytes = 0;
    try_push_bounded_path(
        &mut targets,
        path.to_path_buf(),
        maintenance_path_utf16_units(path),
        &mut path_payload_bytes,
        MAX_MAINTENANCE_TREE_NODES,
        MAX_MAINTENANCE_PATH_BYTES,
        "exact ACL application targets",
    )?;
    if recursive {
        validate_tree(path)?;
        let mut index = 0;
        while index < targets.len() {
            let current = targets[index].clone();
            if fs::symlink_metadata(&current)?.is_dir() {
                for entry in fs::read_dir(&current)? {
                    let target = entry?.path();
                    let path_utf16_units = maintenance_path_utf16_units(&target);
                    try_push_bounded_path(
                        &mut targets,
                        target,
                        path_utf16_units,
                        &mut path_payload_bytes,
                        MAX_MAINTENANCE_TREE_NODES,
                        MAX_MAINTENANCE_PATH_BYTES,
                        "exact ACL application targets",
                    )?;
                }
            }
            index += 1;
        }
    }
    // The collected order is parent-first. Reverse it so every descendant is
    // independently validated and protected before its parent.
    for target in targets.into_iter().rev() {
        set_managed_security_descriptor(&target, &descriptor)?;
    }
    Ok(())
}

fn save_acl(root: &Path, destination: &Path) -> anyhow::Result<()> {
    validate_tree(root)?;
    let mut pending = Vec::new();
    let mut discovered = 0;
    let mut traversal_path_payload_bytes = 0;
    enqueue_bounded_tree_path(
        &mut pending,
        &mut discovered,
        root.to_path_buf(),
        maintenance_path_utf16_units(root),
        &mut traversal_path_payload_bytes,
        MAX_MAINTENANCE_TREE_NODES,
        MAX_MAINTENANCE_PATH_BYTES,
        "ACL snapshot traversal",
    )?;
    let mut entries = Vec::new();
    let mut payload_bytes = 0;
    while let Some(target) = pending.pop() {
        let metadata = fs::symlink_metadata(&target)?;
        if metadata.is_dir() {
            for child in fs::read_dir(&target)? {
                let child = child?.path();
                let path_utf16_units = maintenance_path_utf16_units(&child);
                enqueue_bounded_tree_path(
                    &mut pending,
                    &mut discovered,
                    child,
                    path_utf16_units,
                    &mut traversal_path_payload_bytes,
                    MAX_MAINTENANCE_TREE_NODES,
                    MAX_MAINTENANCE_PATH_BYTES,
                    "ACL snapshot traversal",
                )?;
            }
        }
        let relative = target
            .strip_prefix(root)
            .with_context(|| format!("{} escaped the fixed managed root", target.display()))?;
        let relative_path_utf16 = relative.as_os_str().encode_wide().collect::<Vec<_>>();
        let sddl = security_descriptor_sddl(&target)?;
        let relative_path_utf16_units = relative_path_utf16.len();
        let sddl_bytes = sddl.len();
        try_push_bounded_acl_snapshot_entry(
            &mut entries,
            AclSnapshotEntry {
                relative_path_utf16,
                is_directory: metadata.is_dir(),
                sddl,
            },
            relative_path_utf16_units,
            sddl_bytes,
            &mut payload_bytes,
            MAX_ACL_SNAPSHOT_ENTRIES,
            MAX_ACL_SNAPSHOT_BYTES,
            "ACL snapshot entries",
        )?;
    }
    entries.sort_by(|left, right| left.relative_path_utf16.cmp(&right.relative_path_utf16));
    let snapshot = AclSnapshot {
        format: SNAPSHOT_FORMAT,
        application_version: env!("CARGO_PKG_VERSION").to_owned(),
        entries,
    };
    write_new_private(destination, &serialize_acl_snapshot_bounded(&snapshot)?)
}

struct BoundedAclSnapshotWriter {
    bytes: Vec<u8>,
}

impl std::io::Write for BoundedAclSnapshotWriter {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let requested = self
            .bytes
            .len()
            .checked_add(input.len())
            .ok_or_else(|| std::io::Error::other("ACL snapshot size overflowed"))?;
        if requested > MAX_ACL_SNAPSHOT_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("ACL snapshot exceeds the {MAX_ACL_SNAPSHOT_BYTES}-byte hard limit"),
            ));
        }
        self.bytes.try_reserve(input.len()).map_err(|error| {
            std::io::Error::other(format!(
                "failed to reserve ACL snapshot bytes within the {MAX_ACL_SNAPSHOT_BYTES}-byte hard limit: {error}"
            ))
        })?;
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialize_acl_snapshot_bounded(snapshot: &AclSnapshot) -> anyhow::Result<Vec<u8>> {
    let mut writer = BoundedAclSnapshotWriter { bytes: Vec::new() };
    serde_json::to_writer(&mut writer, snapshot)
        .context("failed to serialize bounded ACL snapshot")?;
    Ok(writer.bytes)
}

fn restore_acl(root: &Path, source: &Path) -> anyhow::Result<()> {
    let snapshot_bytes = read_file_bounded(source, MAX_ACL_SNAPSHOT_BYTES, "ACL snapshot")?;
    let snapshot: AclSnapshot = serde_json::from_slice(&snapshot_bytes)?;
    ensure!(
        snapshot.format == SNAPSHOT_FORMAT
            && snapshot.application_version == env!("CARGO_PKG_VERSION"),
        "unsupported ACL snapshot version"
    );
    ensure!(
        snapshot.entries.len() <= MAX_ACL_SNAPSHOT_ENTRIES,
        "ACL snapshot exceeds the {MAX_ACL_SNAPSHOT_ENTRIES}-entry hard limit"
    );

    let mut snapshot_facts = Vec::new();
    try_reserve_bounded(
        &mut snapshot_facts,
        snapshot.entries.len(),
        MAX_ACL_SNAPSHOT_ENTRIES,
        "ACL snapshot restore facts",
    )?;
    let mut restore_entries = Vec::new();
    try_reserve_bounded(
        &mut restore_entries,
        snapshot.entries.len(),
        MAX_ACL_SNAPSHOT_ENTRIES,
        "ACL snapshot restore entries",
    )?;
    let mut restore_path_payload_bytes = 0;
    for entry in snapshot.entries {
        let relative_path_utf16_units = entry.relative_path_utf16.len();
        let relative = PathBuf::from(OsString::from_wide(&entry.relative_path_utf16));
        let valid_relative_path = relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
        let depth = relative.components().count();
        let target = if relative.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(&relative)
        };
        validate_saved_security_descriptor(&entry.sddl)?;
        try_push_bounded_path(
            &mut snapshot_facts,
            AclSnapshotPathFact {
                path_key: entry.relative_path_utf16,
                depth,
                valid_relative_path,
                is_directory: entry.is_directory,
            },
            relative_path_utf16_units,
            &mut restore_path_payload_bytes,
            MAX_ACL_SNAPSHOT_ENTRIES,
            MAX_MAINTENANCE_PATH_BYTES,
            "ACL snapshot restore facts",
        )?;
        let target_utf16_units = maintenance_path_utf16_units(&target);
        try_push_bounded_path(
            &mut restore_entries,
            (target, entry.sddl),
            target_utf16_units,
            &mut restore_path_payload_bytes,
            MAX_ACL_SNAPSHOT_ENTRIES,
            MAX_MAINTENANCE_PATH_BYTES,
            "ACL snapshot restore targets",
        )?;
    }

    let current_facts = collect_current_acl_path_facts(root)?;
    run_validated_acl_restore_plan(&snapshot_facts, &current_facts, |index| {
        let (target, sddl) = &restore_entries[index];
        restore_security_descriptor(target, sddl)?;
        ensure!(
            security_descriptor_sddl(target)? == *sddl,
            "restored owner/DACL did not verify for {}",
            target.display()
        );
        Ok(())
    })
}

fn collect_current_acl_path_facts(root: &Path) -> anyhow::Result<Vec<AclCurrentPathFact>> {
    let mut facts = Vec::new();
    let mut pending = Vec::new();
    let mut discovered = 0;
    let mut path_payload_bytes = 0;
    enqueue_bounded_tree_path(
        &mut pending,
        &mut discovered,
        root.to_path_buf(),
        maintenance_path_utf16_units(root),
        &mut path_payload_bytes,
        MAX_MAINTENANCE_TREE_NODES,
        MAX_MAINTENANCE_PATH_BYTES,
        "ACL restore tree traversal",
    )?;
    while let Some(target) = pending.pop() {
        let metadata = fs::symlink_metadata(&target).with_context(|| {
            format!("failed to inspect ACL restore target {}", target.display())
        })?;
        let is_reparse_point = metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0;
        let is_directory = metadata.is_dir();
        let is_regular_file = metadata.is_file();
        let hard_link_count = if is_regular_file && !is_reparse_point {
            Some(u64::from(file_link_count(&target)?))
        } else {
            None
        };
        let relative = target
            .strip_prefix(root)
            .with_context(|| format!("{} escaped the fixed managed root", target.display()))?;
        let path_key = relative.as_os_str().encode_wide().collect::<Vec<_>>();
        let relative_path_utf16_units = path_key.len();
        try_push_bounded_path(
            &mut facts,
            AclCurrentPathFact {
                path_key,
                is_directory,
                is_regular_file,
                is_reparse_point,
                hard_link_count,
            },
            relative_path_utf16_units,
            &mut path_payload_bytes,
            MAX_MAINTENANCE_TREE_NODES,
            MAX_MAINTENANCE_PATH_BYTES,
            "ACL restore tree facts",
        )?;

        // Never traverse a name-surrogate directory. Its manifest fact will
        // make validation fail before any ACL is restored.
        if is_directory && !is_reparse_point {
            for child in fs::read_dir(&target)
                .with_context(|| format!("failed to enumerate {}", target.display()))?
            {
                let child = child?.path();
                let path_utf16_units = maintenance_path_utf16_units(&child);
                enqueue_bounded_tree_path(
                    &mut pending,
                    &mut discovered,
                    child,
                    path_utf16_units,
                    &mut path_payload_bytes,
                    MAX_MAINTENANCE_TREE_NODES,
                    MAX_MAINTENANCE_PATH_BYTES,
                    "ACL restore tree traversal",
                )?;
            }
        }
    }
    Ok(facts)
}

fn validate_managed_dacl(path: &Path, require_service_access: bool) -> anyhow::Result<()> {
    let (sddl, is_directory) = security_descriptor_sddl_with_target_type(path)?;
    let service_sid = service_sid_string()?;
    parse_managed_dacl(
        &sddl,
        &service_sid,
        require_service_access,
        is_directory,
    )
}

struct AclTargetHandle {
    raw: HANDLE,
    is_directory: bool,
}

impl Drop for AclTargetHandle {
    fn drop(&mut self) {
        if !self.raw.is_invalid() {
            let _ = unsafe { CloseHandle(self.raw) };
        }
    }
}

struct LocalWideString(PWSTR);

impl Drop for LocalWideString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0.cast()))) };
        }
    }
}

fn open_acl_target(
    path: &Path,
    desired_access: u32,
    operation: &str,
) -> anyhow::Result<AclTargetHandle> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect ACL target {}", path.display()))?;
    let attributes = metadata.file_attributes();
    let expected_directory = attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
    ensure!(
        expected_directory
            || metadata.is_file()
            || attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0,
        "ACL target is neither a directory nor a regular file: {}",
        path.display()
    );

    let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
    if expected_directory {
        flags |= FILE_FLAG_BACKUP_SEMANTICS;
    }
    let wide_path = wide_null(path.as_os_str());
    // Deliberately omit FILE_SHARE_DELETE so this handle prevents a delete or
    // rename from replacing the checked object before the ACL operation.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_path.as_ptr()),
            desired_access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            flags,
            None,
        )
    }
    .with_context(|| format!("failed to open {} for {operation}", path.display()))?;
    let mut handle = AclTargetHandle {
        raw: handle,
        is_directory: false,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle.raw, &mut information) }
        .with_context(|| format!("failed to inspect opened ACL target {}", path.display()))?;
    let actual_directory = information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
    validate_opened_managed_target_facts(OpenedManagedTargetFacts {
        expected_directory,
        actual_directory,
        is_reparse_point: information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0,
        hard_link_count: information.nNumberOfLinks,
    })
    .with_context(|| format!("refusing ACL access to {}", path.display()))?;
    handle.is_directory = actual_directory;
    Ok(handle)
}

fn open_acl_target_for_read(path: &Path) -> anyhow::Result<AclTargetHandle> {
    open_acl_target(path, READ_CONTROL.0, "security descriptor read")
}

fn open_acl_target_for_write(path: &Path) -> anyhow::Result<AclTargetHandle> {
    open_acl_target(
        path,
        WRITE_DAC.0 | WRITE_OWNER.0,
        "security descriptor update",
    )
}

fn check_security_status(
    status: windows::Win32::Foundation::WIN32_ERROR,
    operation: &str,
    path: &Path,
) -> anyhow::Result<()> {
    if status == ERROR_SUCCESS {
        return Ok(());
    }
    Err(std::io::Error::from_raw_os_error(status.0 as i32))
        .with_context(|| format!("{operation} failed for {}", path.display()))
}

fn security_descriptor_sddl_with_target_type(path: &Path) -> anyhow::Result<(String, bool)> {
    let handle = open_acl_target_for_read(path)?;
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let status = unsafe {
        GetSecurityInfo(
            handle.raw,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    };
    let descriptor = LocalSecurityDescriptor(descriptor);
    check_security_status(status, "GetSecurityInfo", path)?;
    ensure!(
        !descriptor.0.is_invalid(),
        "GetSecurityInfo returned no security descriptor for {}",
        path.display()
    );

    let mut text = PWSTR::null();
    let mut length = 0;
    let conversion = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor.0,
            SDDL_REVISION_1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut text,
            Some(&mut length),
        )
    };
    let text = LocalWideString(text);
    conversion.with_context(|| {
        format!(
            "failed to convert the security descriptor for {} to SDDL",
            path.display()
        )
    })?;
    ensure!(
        !text.0.is_null(),
        "security descriptor conversion returned no text for {}",
        path.display()
    );
    let utf16 = unsafe { std::slice::from_raw_parts(text.0.as_ptr(), length as usize) };
    let content_length = utf16
        .iter()
        .position(|code_unit| *code_unit == 0)
        .unwrap_or(utf16.len());
    let sddl = String::from_utf16(&utf16[..content_length]).with_context(|| {
        format!(
            "security descriptor for {} is not valid UTF-16",
            path.display()
        )
    })?;
    Ok((sddl, handle.is_directory))
}

fn security_descriptor_sddl(path: &Path) -> anyhow::Result<String> {
    security_descriptor_sddl_with_target_type(path).map(|(sddl, _)| sddl)
}

fn set_managed_security_descriptor(path: &Path, sddl: &str) -> anyhow::Result<()> {
    set_security_descriptor(path, sddl, true, true)
}

fn saved_dacl_is_protected(sddl: &str) -> anyhow::Result<bool> {
    let dacl = sddl
        .split_once("D:")
        .map(|(_, value)| value)
        .context("saved security descriptor has no DACL")?;
    let control = dacl.split_once('(').map(|(value, _)| value).unwrap_or(dacl);
    Ok(control.contains('P'))
}

fn validate_saved_security_descriptor(sddl: &str) -> anyhow::Result<()> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR,
        },
    };

    saved_dacl_is_protected(sddl)?;
    let wide_sddl = wide_null(OsStr::new(sddl));
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide_sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    ensure!(
        converted != 0,
        "failed to validate a saved Client security descriptor"
    );
    unsafe { LocalFree(descriptor.cast()) };
    Ok(())
}

fn restore_security_descriptor(path: &Path, sddl: &str) -> anyhow::Result<()> {
    set_security_descriptor(path, sddl, saved_dacl_is_protected(sddl)?, false)
}

fn set_security_descriptor(
    path: &Path,
    sddl: &str,
    protected: bool,
    managed_template: bool,
) -> anyhow::Result<()> {
    let handle = open_acl_target_for_write(path)?;
    let target_sddl = if managed_template {
        managed_security_descriptor_for_target(sddl, handle.is_directory)
    } else {
        std::borrow::Cow::Borrowed(sddl)
    };
    let wide_sddl = wide_null(OsStr::new(target_sddl.as_ref()));
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let conversion = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide_sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    };
    let descriptor = LocalSecurityDescriptor(descriptor);
    conversion.context("failed to build the exact Client security descriptor")?;

    let wide_path = wide_null(path.as_os_str());
    let applied = unsafe {
        SetFileSecurityW(
            PCWSTR(wide_path.as_ptr()),
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | if protected {
                    PROTECTED_DACL_SECURITY_INFORMATION
                } else {
                    UNPROTECTED_DACL_SECURITY_INFORMATION
                },
            descriptor.0,
        )
    };
    let result = applied
        .ok()
        .with_context(|| format!("SetFileSecurityW failed for {}", path.display()));
    // Keep the already validated, non-delete-shared target open through the
    // path-based API call so the checked directory entry cannot be replaced.
    drop(handle);
    result
}
