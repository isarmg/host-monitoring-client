fn maintenance_path_utf16_units(path: &Path) -> usize {
    path.as_os_str().encode_wide().count()
}

fn validate_optional_root(path: &Path, label: &str) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            validate_real_directory(path, label)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {label}")),
    }
}

fn validate_real_directory(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    ensure!(
        metadata.is_dir(),
        "{label} is not a directory: {}",
        path.display()
    );
    ensure!(
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
        "{label} is a reparse point: {}",
        path.display()
    );
    Ok(())
}

fn validate_regular_single_link(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    ensure!(metadata.is_file(), "{label} is not a regular file");
    ensure!(
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
        "{label} is a reparse point"
    );
    ensure!(
        file_link_count(path)? == 1,
        "{label} has multiple hard links"
    );
    Ok(())
}

fn validate_tree(root: &Path) -> anyhow::Result<()> {
    validate_tree_with_directory_depth_limit(root, None)
}

fn validate_tree_with_directory_depth_limit(
    root: &Path,
    directory_depth_limit: Option<usize>,
) -> anyhow::Result<()> {
    validate_real_directory(root, "managed directory")?;
    if let Some(hard_limit) = directory_depth_limit {
        ensure!(
            hard_limit >= 1,
            "managed directory depth hard limit must include the root"
        );
    }
    let mut pending = Vec::new();
    let mut discovered = 0;
    let mut path_payload_bytes = 0;
    enqueue_bounded_tree_path(
        &mut pending,
        &mut discovered,
        (root.to_path_buf(), 1usize),
        maintenance_path_utf16_units(root),
        &mut path_payload_bytes,
        MAX_MAINTENANCE_TREE_NODES,
        MAX_MAINTENANCE_PATH_BYTES,
        "managed tree traversal",
    )?;
    while let Some((directory, directory_depth)) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to enumerate {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
                "managed tree contains a reparse point: {}",
                path.display()
            );
            if metadata.is_dir() {
                let child_depth = match directory_depth_limit {
                    Some(hard_limit) => checked_child_directory_depth(directory_depth, hard_limit)?,
                    None => directory_depth
                        .checked_add(1)
                        .context("managed directory depth overflowed")?,
                };
                let path_utf16_units = maintenance_path_utf16_units(&path);
                enqueue_bounded_tree_path(
                    &mut pending,
                    &mut discovered,
                    (path, child_depth),
                    path_utf16_units,
                    &mut path_payload_bytes,
                    MAX_MAINTENANCE_TREE_NODES,
                    MAX_MAINTENANCE_PATH_BYTES,
                    "managed tree traversal queue",
                )?;
            } else {
                record_bounded_tree_node(
                    &mut discovered,
                    MAX_MAINTENANCE_TREE_NODES,
                    "managed tree traversal",
                )?;
                ensure!(
                    metadata.is_file(),
                    "managed tree contains a special file: {}",
                    path.display()
                );
                ensure!(
                    file_link_count(&path)? == 1,
                    "managed tree contains a multiply-linked file: {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

fn ensure_absent(path: &Path, label: &str) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => bail!("{label} already exists at {}", path.display()),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {label}")),
    }
}

struct MutationTargetHandle(HANDLE);

impl Drop for MutationTargetHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

fn open_mutation_target_with_access(
    path: &Path,
    expected_directory: bool,
    desired_access: u32,
    operation: &str,
) -> anyhow::Result<MutationTargetHandle> {
    let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
    if expected_directory {
        flags |= FILE_FLAG_BACKUP_SEMANTICS;
    }
    let wide_path = wide_null(path.as_os_str());
    // Omitting FILE_SHARE_DELETE prevents the validated object from being
    // replaced before the handle-bound mutation completes.
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
    let handle = MutationTargetHandle(handle);

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle.0, &mut information) }.with_context(|| {
        format!(
            "failed to inspect opened mutation target {}",
            path.display()
        )
    })?;
    validate_opened_managed_target_facts(OpenedManagedTargetFacts {
        expected_directory,
        actual_directory: information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
        is_reparse_point: information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0,
        hard_link_count: information.nNumberOfLinks,
    })
    .with_context(|| format!("refusing {operation} for {}", path.display()))?;
    Ok(handle)
}

fn open_mutation_target(
    path: &Path,
    expected_directory: bool,
    operation: &str,
) -> anyhow::Result<MutationTargetHandle> {
    // DELETE access permits rename and disposition changes on this target.
    open_mutation_target_with_access(
        path,
        expected_directory,
        DELETE.0 | FILE_READ_ATTRIBUTES.0,
        operation,
    )
}

fn open_rename_parent(path: &Path) -> anyhow::Result<MutationTargetHandle> {
    open_mutation_target_with_access(
        path,
        true,
        FILE_TRAVERSE.0 | FILE_READ_ATTRIBUTES.0,
        "managed rename parent directory",
    )
}

fn delete_opened_mutation_target(handle: MutationTargetHandle, path: &Path) -> anyhow::Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    unsafe {
        SetFileInformationByHandle(
            handle.0,
            FileDispositionInfo,
            (&raw const disposition).cast(),
            u32::try_from(size_of::<FILE_DISPOSITION_INFO>())
                .context("FILE_DISPOSITION_INFO size does not fit in a DWORD")?,
        )
    }
    .with_context(|| {
        format!(
            "failed to mark {} for handle-bound deletion",
            path.display()
        )
    })?;
    drop(handle);
    Ok(())
}

fn remove_empty_directory_by_handle(path: &Path, label: &str) -> anyhow::Result<()> {
    let handle = open_mutation_target(path, true, "empty managed directory removal")?;
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("failed to inspect whether {label} is empty"))?;
    match entries.next() {
        None => {}
        Some(Ok(_)) => bail!("{label} is not empty: {}", path.display()),
        Some(Err(error)) => {
            return Err(error).with_context(|| format!("failed to enumerate {label}"));
        }
    }
    drop(entries);
    delete_opened_mutation_target(handle, path)?;
    ensure_absent(path, &format!("{label} after handle-bound deletion"))
}

struct RemovalDirectoryFrame {
    path: PathBuf,
    handle: MutationTargetHandle,
    entries: fs::ReadDir,
}

fn removal_directory_frame(
    path: PathBuf,
    handle: MutationTargetHandle,
) -> anyhow::Result<RemovalDirectoryFrame> {
    let entries = fs::read_dir(&path)
        .with_context(|| format!("failed to enumerate {} for removal", path.display()))?;
    Ok(RemovalDirectoryFrame {
        path,
        handle,
        entries,
    })
}

fn remove_tree_no_reparse(root: &Path) -> anyhow::Result<()> {
    let root_handle = open_mutation_target(root, true, "managed tree removal")?;
    // Preserve the existing fail-before-mutation validation while the root
    // handle prevents the tree root from being renamed or replaced. Every
    // final mutation is still revalidated against its own opened handle.
    validate_tree_with_directory_depth_limit(root, Some(MAX_OPEN_MUTATION_DIRECTORIES))?;
    let root_frame = removal_directory_frame(root.to_path_buf(), root_handle)?;
    let mut stack = Vec::new();
    let mut discovered = 0;
    let mut path_payload_bytes = 0;
    record_bounded_tree_node(
        &mut discovered,
        MAX_MAINTENANCE_TREE_NODES,
        "managed tree removal",
    )?;
    try_push_bounded_path(
        &mut stack,
        root_frame,
        maintenance_path_utf16_units(root),
        &mut path_payload_bytes,
        MAX_OPEN_MUTATION_DIRECTORIES,
        MAX_MAINTENANCE_PATH_BYTES,
        "managed tree removal directory handle stack",
    )?;

    while !stack.is_empty() {
        let next = stack
            .last_mut()
            .expect("removal stack was checked as non-empty")
            .entries
            .next();
        match next {
            Some(entry) => {
                let path = entry?.path();
                let metadata = fs::symlink_metadata(&path).with_context(|| {
                    format!(
                        "failed to inspect managed removal target {}",
                        path.display()
                    )
                })?;
                let attributes = metadata.file_attributes();
                let expected_directory = attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
                ensure!(
                    expected_directory
                        || metadata.is_file()
                        || attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0,
                    "managed tree contains a special file: {}",
                    path.display()
                );
                record_bounded_tree_node(
                    &mut discovered,
                    MAX_MAINTENANCE_TREE_NODES,
                    "managed tree removal",
                )?;
                let handle =
                    open_mutation_target(&path, expected_directory, "managed tree removal")?;
                if expected_directory {
                    let path_utf16_units = maintenance_path_utf16_units(&path);
                    let frame = removal_directory_frame(path, handle)?;
                    try_push_bounded_path(
                        &mut stack,
                        frame,
                        path_utf16_units,
                        &mut path_payload_bytes,
                        MAX_OPEN_MUTATION_DIRECTORIES,
                        MAX_MAINTENANCE_PATH_BYTES,
                        "managed tree removal directory handle stack",
                    )?;
                } else {
                    delete_opened_mutation_target(handle, &path)?;
                }
            }
            None => {
                let RemovalDirectoryFrame {
                    path,
                    handle,
                    entries,
                } = stack.pop().expect("removal stack was checked as non-empty");
                drop(entries);
                delete_opened_mutation_target(handle, &path)?;
            }
        }
    }
    ensure_absent(root, "managed tree root after handle-bound deletion")
}

fn rename_managed_directory_by_handle(
    source: &Path,
    destination: &Path,
    destination_label: &str,
) -> anyhow::Result<()> {
    ensure!(
        source.is_absolute(),
        "managed rename source is not absolute: {}",
        source.display()
    );
    ensure!(
        destination.is_absolute(),
        "managed rename destination is not absolute: {}",
        destination.display()
    );
    ensure!(
        source != destination,
        "managed rename source and destination are identical"
    );
    let source_parent = source
        .parent()
        .context("managed rename source has no parent directory")?;
    ensure!(
        destination.parent() == Some(source_parent),
        "managed rename destination must share the source parent directory"
    );
    let destination_name = destination
        .file_name()
        .context("managed rename destination has no file name")?;
    let mut destination_components = Path::new(destination_name).components();
    ensure!(
        matches!(destination_components.next(), Some(Component::Normal(_)))
            && destination_components.next().is_none(),
        "managed rename destination file name is not a single normal component"
    );
    ensure!(
        !destination_name
            .encode_wide()
            .any(|code_unit| code_unit == u16::from(b':')),
        "managed rename destination contains an alternate-stream separator"
    );
    // Lock and validate the fixed parent while Win32 receives the explicit
    // absolute destination. The passed buffer includes its terminating NUL.
    let parent_handle = open_rename_parent(source_parent)?;
    ensure_absent(destination, destination_label)?;
    let handle = open_mutation_target(source, true, "managed directory rename")?;

    let mut file_name_utf16_units = 0usize;
    for code_unit in destination.as_os_str().encode_wide() {
        ensure!(
            code_unit != 0,
            "managed rename destination contains an embedded NUL"
        );
        file_name_utf16_units = file_name_utf16_units
            .checked_add(1)
            .context("managed rename destination length overflowed")?;
    }
    let file_name_offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let plan = checked_rename_buffer_plan(
        file_name_utf16_units,
        file_name_offset,
        size_of::<usize>(),
        MAX_MAINTENANCE_PATH_BYTES,
    )?;

    // FILE_RENAME_INFO has an inline flexible UTF-16 tail. usize backing
    // provides sufficient alignment for both its HANDLE and union fields.
    ensure!(
        std::mem::align_of::<FILE_RENAME_INFO>() <= std::mem::align_of::<usize>(),
        "FILE_RENAME_INFO requires unsupported storage alignment"
    );
    let mut storage = Vec::<usize>::new();
    storage
        .try_reserve_exact(plan.storage_words)
        .context("failed to reserve the bounded rename information buffer")?;
    storage.resize(plan.storage_words, 0);
    let allocated_bytes = checked_rename_storage_bytes(
        plan,
        size_of::<usize>(),
        size_of::<FILE_RENAME_INFO>(),
        file_name_offset,
    )?;
    ensure!(
        storage
            .len()
            .checked_mul(size_of::<usize>())
            .context("rename information Vec size overflowed")?
            == allocated_bytes,
        "rename information allocation does not match its checked plan"
    );
    let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        ptr::write(information, FILE_RENAME_INFO::default());
        (*information).Anonymous.ReplaceIfExists = false;
        (*information).RootDirectory = HANDLE::default();
        (*information).FileNameLength = plan.file_name_bytes;
        let file_name = storage
            .as_mut_ptr()
            .cast::<u8>()
            .add(file_name_offset)
            .cast::<u16>();
        for (index, code_unit) in destination.as_os_str().encode_wide().enumerate() {
            file_name.add(index).write(code_unit);
        }
        SetFileInformationByHandle(
            handle.0,
            FileRenameInfo,
            information.cast(),
            plan.buffer_bytes,
        )
    }
    .with_context(|| {
        format!(
            "failed to rename {} to fixed destination {}",
            source.display(),
            destination.display()
        )
    })?;
    ensure_absent(source, "managed rename source after handle-bound rename")?;
    validate_real_directory(destination, destination_label)?;
    drop(handle);
    drop(parent_handle);
    Ok(())
}

fn write_new_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let result = file.write_all(bytes).and_then(|_| file.sync_all());
    drop(file);
    if let Err(error) = result {
        let _ = fs::remove_file(path);
        return Err(error).with_context(|| format!("failed to write {}", path.display()));
    }
    Ok(())
}

struct DiagnosticFileHandle {
    handle: HANDLE,
    cleanup_required: bool,
}

impl Drop for DiagnosticFileHandle {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            if self.cleanup_required {
                let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
                let _ = unsafe {
                    SetFileInformationByHandle(
                        self.handle,
                        FileDispositionInfo,
                        (&raw const disposition).cast(),
                        u32::try_from(size_of::<FILE_DISPOSITION_INFO>()).unwrap_or(u32::MAX),
                    )
                };
            }
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
}

fn rename_diagnostic_to_final(handle: HANDLE, destination: &Path) -> anyhow::Result<()> {
    ensure!(
        destination.is_absolute(),
        "maintenance diagnostic destination is not absolute"
    );
    let mut file_name_utf16_units = 0usize;
    for code_unit in destination.as_os_str().encode_wide() {
        ensure!(
            code_unit != 0,
            "maintenance diagnostic destination contains an embedded NUL"
        );
        file_name_utf16_units = file_name_utf16_units
            .checked_add(1)
            .context("maintenance diagnostic destination length overflowed")?;
    }
    let file_name_offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let plan = checked_rename_buffer_plan(
        file_name_utf16_units,
        file_name_offset,
        size_of::<usize>(),
        MAX_MAINTENANCE_PATH_BYTES,
    )?;
    ensure!(
        std::mem::align_of::<FILE_RENAME_INFO>() <= std::mem::align_of::<usize>(),
        "FILE_RENAME_INFO requires unsupported storage alignment"
    );
    let mut storage = Vec::<usize>::new();
    storage
        .try_reserve_exact(plan.storage_words)
        .context("failed to reserve the bounded diagnostic rename buffer")?;
    storage.resize(plan.storage_words, 0);
    let allocated_bytes = checked_rename_storage_bytes(
        plan,
        size_of::<usize>(),
        size_of::<FILE_RENAME_INFO>(),
        file_name_offset,
    )?;
    ensure!(
        storage
            .len()
            .checked_mul(size_of::<usize>())
            .context("diagnostic rename information Vec size overflowed")?
            == allocated_bytes,
        "diagnostic rename allocation does not match its checked plan"
    );
    let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        ptr::write(information, FILE_RENAME_INFO::default());
        (*information).Anonymous.ReplaceIfExists = false;
        (*information).RootDirectory = HANDLE::default();
        (*information).FileNameLength = plan.file_name_bytes;
        let file_name = storage
            .as_mut_ptr()
            .cast::<u8>()
            .add(file_name_offset)
            .cast::<u16>();
        for (index, code_unit) in destination.as_os_str().encode_wide().enumerate() {
            file_name.add(index).write(code_unit);
        }
        SetFileInformationByHandle(
            handle,
            FileRenameInfo,
            information.cast(),
            plan.buffer_bytes,
        )
    }
    .with_context(|| {
        format!(
            "failed to publish the first complete maintenance diagnostic {}",
            destination.display()
        )
    })
}

fn write_first_maintenance_diagnostic(
    path: &Path,
    command: &str,
    error: &anyhow::Error,
) -> anyhow::Result<()> {
    let payload = maintenance_diagnostic_payload(command, error);
    ensure!(
        payload.len() <= MAINTENANCE_DIAGNOSTIC_MAX_BYTES,
        "maintenance diagnostic payload exceeded its hard limit"
    );

    let descriptor_text = wide_null(OsStr::new(MAINTENANCE_DIAGNOSTIC_SDDL));
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(descriptor_text.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .context("failed to build the maintenance diagnostic security descriptor")?;
    let descriptor = LocalSecurityDescriptor(descriptor);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .context("SECURITY_ATTRIBUTES size does not fit in a DWORD")?,
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: false.into(),
    };
    let staging_path = path.with_extension(format!("{}.pending", uuid::Uuid::new_v4()));
    let wide_staging_path = wide_null(staging_path.as_os_str());
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_staging_path.as_ptr()),
            GENERIC_WRITE.0 | DELETE.0,
            FILE_SHARE_MODE::default(),
            Some(&attributes),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .with_context(|| {
        format!(
            "failed to create a private maintenance diagnostic staging file beside {}",
            path.display(),
        )
    })?;
    let mut handle = DiagnosticFileHandle {
        handle,
        cleanup_required: true,
    };

    let mut remaining = payload.as_slice();
    while !remaining.is_empty() {
        let mut written = 0;
        unsafe { WriteFile(handle.handle, Some(remaining), Some(&mut written), None) }
            .context("failed to write the maintenance diagnostic")?;
        ensure!(written != 0, "maintenance diagnostic write made no progress");
        let written = usize::try_from(written)?;
        ensure!(
            written <= remaining.len(),
            "maintenance diagnostic write exceeded its input buffer"
        );
        remaining = &remaining[written..];
    }
    unsafe { FlushFileBuffers(handle.handle) }
        .context("failed to flush the maintenance diagnostic")?;
    rename_diagnostic_to_final(handle.handle, path)?;
    handle.cleanup_required = false;
    Ok(())
}

fn replace_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let replacement = path.with_extension("replacement");
    ensure_absent(&replacement, "stale private replacement file")?;
    write_new_private(&replacement, bytes)?;
    let source = wide_null(replacement.as_os_str());
    let destination = wide_null(path.as_os_str());
    let replaced = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        let error = std::io::Error::last_os_error();
        let _ = fs::remove_file(&replacement);
        return Err(error).with_context(|| format!("failed to replace {}", path.display()));
    }
    Ok(())
}

fn read_snapshot(paths: &FixedPaths) -> anyhow::Result<InstallSnapshot> {
    validate_real_directory(&paths.journal_root, "install journal")?;
    validate_tree(&paths.journal_root)?;
    validate_managed_dacl(&paths.journal_root, false)?;
    let snapshot: InstallSnapshot =
        serde_json::from_slice(&fs::read(paths.journal_root.join(SNAPSHOT_FILE))?)?;
    ensure!(
        snapshot.format == SNAPSHOT_FORMAT
            && snapshot.application_version == env!("CARGO_PKG_VERSION"),
        "unsupported maintenance journal version"
    );
    Ok(snapshot)
}

fn read_uninstall_snapshot(paths: &FixedPaths) -> anyhow::Result<UninstallSnapshot> {
    validate_real_directory(&paths.uninstall_journal_root, "uninstall journal")?;
    validate_tree(&paths.uninstall_journal_root)?;
    validate_managed_dacl(&paths.uninstall_journal_root, false)?;
    let snapshot: UninstallSnapshot =
        serde_json::from_slice(&fs::read(paths.uninstall_journal_root.join(SNAPSHOT_FILE))?)?;
    ensure!(
        snapshot.format == SNAPSHOT_FORMAT
            && snapshot.application_version == env!("CARGO_PKG_VERSION"),
        "unsupported uninstall journal version"
    );
    purge_started(paths)?;
    Ok(snapshot)
}

fn purge_started(paths: &FixedPaths) -> anyhow::Result<bool> {
    let marker = paths.uninstall_journal_root.join(PURGE_STARTED_FILE);
    match fs::read(&marker) {
        Ok(content) => {
            ensure!(
                content == PURGE_STARTED_CONTENT.as_bytes(),
                "purge transaction marker does not belong to the current package"
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("failed to read purge transaction marker"),
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

fn create_system_admin_only_directory(path: &Path, label: &str) -> anyhow::Result<()> {
    let descriptor_text = protected_directory_security_descriptor();
    let wide_descriptor = wide_null(OsStr::new(&descriptor_text));
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide_descriptor.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .context("failed to build the protected directory security descriptor")?;
    let descriptor = LocalSecurityDescriptor(descriptor);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .context("SECURITY_ATTRIBUTES size does not fit in a DWORD")?,
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: false.into(),
    };
    let wide_path = wide_null(path.as_os_str());
    unsafe { CreateDirectoryW(PCWSTR(wide_path.as_ptr()), Some(&attributes)) }
        .with_context(|| format!("failed to create protected {label} {}", path.display()))?;

    if let Err(error) =
        validate_real_directory(path, label).and_then(|_| validate_managed_dacl(path, false))
    {
        let cleanup = remove_tree_no_reparse(path);
        return match cleanup {
            Ok(()) => Err(error.context(format!("failed to verify protected {label}"))),
            Err(cleanup_error) => Err(error.context(format!(
                "failed to verify protected {label}; cleanup also failed: {cleanup_error:#}"
            ))),
        };
    }
    Ok(())
}

fn create_protected_journal(path: &Path) -> anyhow::Result<()> {
    create_system_admin_only_directory(path, "new maintenance journal")
}

fn write_or_validate_state_marker(paths: &FixedPaths) -> anyhow::Result<()> {
    validate_real_directory(
        &paths.state_root,
        "state root required before marker update",
    )?;
    let marker = paths.state_root.join(STATE_MARKER);
    if marker.exists() {
        validate_marker_file(&marker)?;
    } else {
        write_new_private(&marker, STATE_MARKER_CONTENT.as_bytes())?;
        let service_sid = service_sid_string()?;
        if let Err(error) = apply_exact_acl(&marker, Some(&service_sid), false)
            .and_then(|_| validate_managed_dacl(&marker, true))
        {
            let _ = fs::remove_file(&marker);
            return Err(error.context("failed to secure the new state marker"));
        }
    }
    Ok(())
}

fn validate_state_marker(paths: &FixedPaths, require_service_access: bool) -> anyhow::Result<()> {
    let marker = paths.state_root.join(STATE_MARKER);
    validate_marker_file(&marker)?;
    validate_managed_dacl(&paths.state_root, require_service_access)?;
    validate_managed_dacl(&marker, require_service_access)
}

fn validate_marker_file(marker: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(marker)
        .with_context(|| format!("missing trusted state marker {}", marker.display()))?;
    ensure!(metadata.is_file(), "state marker is not a regular file");
    ensure!(
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
        "state marker is a reparse point"
    );
    ensure!(
        file_link_count(marker)? == 1,
        "state marker has multiple hard links"
    );
    ensure!(
        fs::read(marker)? == STATE_MARKER_CONTENT.as_bytes(),
        "state marker content is invalid"
    );
    Ok(())
}
