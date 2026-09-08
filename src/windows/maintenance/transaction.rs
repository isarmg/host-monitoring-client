fn prepare_install(paths: &FixedPaths) -> anyhow::Result<()> {
    ensure_absent(&paths.journal_root, "stale install journal")?;
    ensure_absent(&paths.uninstall_journal_root, "pending uninstall journal")?;
    ensure_absent(&paths.quarantine_root, "pending purge quarantine")?;
    let program_existed = validate_optional_root(&paths.program_root, "program root")?;
    if program_existed {
        validate_tree(&paths.program_root)?;
    }
    let state_existed = validate_optional_root(&paths.state_root, "state root")?;
    if state_existed {
        validate_tree(&paths.state_root)?;
    }
    let existing_service = open_client_service(SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)?;
    if program_existed {
        validate_program_tree(&paths.program_root)?;
    }
    let original_service_sid_type = if let Some(service) = existing_service.as_ref() {
        validate_client_service(service, paths)?;
        // An interrupted uninstall can leave SCM registration with no files/state.
        // The fixed ImagePath and LocalService identity above establish which
        // service MSI may repair; never require missing payloads to validate.
        if state_existed {
            validate_state_marker(paths, true)?;
        }
        let sid_type = query_service_sid_type(service)?;
        // MSI rolls back service registration itself. Native restoration needs
        // the old payload only when that payload actually existed.
        (program_existed && state_existed).then_some(sid_type)
    } else {
        None
    };
    let original_service_running = existing_service
        .as_ref()
        .map(service_is_active)
        .transpose()?
        .unwrap_or(false);
    let original_failure_actions_on_non_crash = existing_service
        .as_ref()
        .map(query_failure_actions_on_non_crash)
        .transpose()?;
    if state_existed && existing_service.is_none() {
        // A normal uninstall deliberately preserves the current identity
        // under the service-free ACL. The exact current marker and ACL are
        // required before a same-version reinstall may reuse that state.
        validate_state_marker(paths, false)?;
    }

    create_protected_journal(&paths.journal_root)?;

    let result = (|| {
        let snapshot = InstallSnapshot {
            format: SNAPSHOT_FORMAT,
            application_version: env!("CARGO_PKG_VERSION").to_owned(),
            program_existed,
            state_existed,
            original_service_sid_type,
            original_failure_actions_on_non_crash,
            original_service_running,
            state_acl_saved: state_existed,
        };
        write_new_private(
            &paths.journal_root.join(SNAPSHOT_FILE),
            &serde_json::to_vec_pretty(&snapshot)?,
        )?;
        validate_tree(&paths.journal_root)
    })();
    if result.is_err() {
        let _ = remove_tree_no_reparse(&paths.journal_root);
        return result;
    }
    if state_existed {
        save_acl(&paths.state_root, &paths.journal_root.join(STATE_ACL_FILE))?;
    }
    if program_existed {
        save_acl(
            &paths.program_root,
            &paths.journal_root.join(PROGRAM_ACL_FILE),
        )?;
    }
    validate_tree(&paths.journal_root)?;
    Ok(())
}

fn apply_install(paths: &FixedPaths) -> anyhow::Result<()> {
    let snapshot = read_snapshot(paths)?;
    ensure!(
        validate_optional_root(&paths.program_root, "program root")?,
        "MSI did not create the program root"
    );
    validate_tree(&paths.program_root)?;
    validate_regular_single_link(&paths.program_exe, "installed Client executable")?;
    if snapshot.state_existed {
        ensure!(
            validate_optional_root(&paths.state_root, "state root")?,
            "the pre-existing Client state root disappeared before apply"
        );
        validate_tree(&paths.state_root)?;
    } else {
        ensure_absent(
            &paths.state_root,
            "state root that appeared after install preflight",
        )?;
        create_system_admin_only_directory(&paths.state_root, "new Client state root")?;
        write_new_private(
            &paths.config,
            &serde_json::to_vec_pretty(&ClientConfig::default())?,
        )?;
    }

    let service = open_client_service(SERVICE_QUERY_CONFIG | SERVICE_CHANGE_CONFIG)?
        .context("MSI did not install the host-monitor service")?;
    validate_client_service(&service, paths)?;
    set_service_sid_type(&service, SERVICE_SID_TYPE_UNRESTRICTED)?;
    ensure!(
        query_service_sid_type(&service)? == SERVICE_SID_TYPE_UNRESTRICTED,
        "host-monitor service SID type verification failed"
    );
    set_failure_actions_on_non_crash(&service, true)?;
    ensure!(
        query_failure_actions_on_non_crash(&service)?,
        "host-monitor non-crash failure restart policy did not verify"
    );

    secure_program_for_service(&paths.program_root)?;
    validate_program_tree(&paths.program_root)?;

    write_or_validate_state_marker(paths)?;
    validate_tree(&paths.state_root)?;
    secure_state_for_service(&paths.state_root)?;
    validate_state_marker(paths, true)?;
    Ok(())
}

fn rollback_install(paths: &FixedPaths) -> anyhow::Result<()> {
    if !rollback_path_exists(&paths.journal_root, "install journal")? {
        return Ok(());
    }
    validate_real_directory(&paths.journal_root, "install journal")?;
    validate_tree(&paths.journal_root)?;
    let snapshot = read_snapshot(paths)?;
    let mut failures = Vec::new();

    let install_acl_snapshot = paths.journal_root.join(STATE_ACL_FILE);
    if snapshot.state_acl_saved
        && rollback_path_exists(&install_acl_snapshot, "state ACL snapshot")?
        && rollback_path_exists(&paths.state_root, "state root")?
        && let Err(error) = restore_acl(&paths.state_root, &install_acl_snapshot)
    {
        failures.push(format!("restore state ACL: {error:#}"));
    }
    let program_acl_snapshot = paths.journal_root.join(PROGRAM_ACL_FILE);
    if snapshot.program_existed
        && rollback_path_exists(&program_acl_snapshot, "program ACL snapshot")?
        && rollback_path_exists(&paths.program_root, "program root")?
        && let Err(error) = restore_acl(&paths.program_root, &program_acl_snapshot)
    {
        failures.push(format!("restore program ACL: {error:#}"));
    }
    if let Some(sid_type) = snapshot.original_service_sid_type
        && let Err(error) = restore_service_state(
            paths,
            sid_type,
            snapshot
                .original_failure_actions_on_non_crash
                .unwrap_or(true),
            snapshot.original_service_running,
            ProgramAclRestore::PreserveSnapshot,
        )
    {
        failures.push(format!("restore service state: {error:#}"));
    }

    if !snapshot.state_existed
        && rollback_path_exists(&paths.state_root, "fresh-install state root")?
        && let Err(error) = remove_fresh_install_state(paths)
    {
        failures.push(format!("remove fresh-install state: {error:#}"));
    }

    if failures.is_empty() {
        remove_tree_no_reparse(&paths.journal_root)?;
        Ok(())
    } else {
        bail!(
            "rollback was incomplete; the protected journal was retained: {}",
            failures.join("; ")
        )
    }
}

fn commit_install(paths: &FixedPaths) -> anyhow::Result<()> {
    if !paths.journal_root.exists() {
        return Ok(());
    }
    validate_real_directory(&paths.journal_root, "install journal")?;
    validate_tree(&paths.journal_root)?;
    read_snapshot(paths)?;
    remove_tree_no_reparse(&paths.journal_root)
}

fn remove_fresh_install_state(paths: &FixedPaths) -> anyhow::Result<()> {
    validate_real_directory(&paths.state_root, "fresh-install state root")?;
    validate_tree(&paths.state_root)?;
    let marker = paths.state_root.join(STATE_MARKER);
    match fs::symlink_metadata(&marker) {
        Ok(_) => {
            // The root is protected as SYSTEM/Administrators immediately after creation;
            // the marker is then created with the service template before the recursive
            // transition. A failure may therefore leave the root in either exact mode.
            validate_marker_file(&marker)?;
            validate_managed_dacl(&marker, true)?;
            if validate_managed_dacl(&paths.state_root, false).is_err() {
                validate_managed_dacl(&paths.state_root, true).context(
                    "fresh-install state root matches neither exact rollback-safe ACL mode",
                )?;
            }
            remove_tree_no_reparse(&paths.state_root)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if validate_managed_dacl(&paths.state_root, false).is_ok() {
                remove_tree_no_reparse(&paths.state_root)
            } else {
                remove_empty_directory_by_handle(
                    &paths.state_root,
                    "unmarked fresh-install state root not protected by the exact pre-marker ACL",
                )
                .context("failed to remove the empty fresh-install state root by handle")
            }
        }
        Err(error) => Err(error).context("failed to inspect the fresh-install state marker"),
    }
}

fn preflight_uninstall(paths: &FixedPaths) -> anyhow::Result<()> {
    ensure_absent(&paths.uninstall_journal_root, "stale uninstall journal")?;
    ensure!(
        validate_optional_root(&paths.program_root, "program root")?,
        "installed program root is missing"
    );
    validate_regular_single_link(&paths.program_exe, "installed Client executable")?;
    validate_program_tree(&paths.program_root)?;
    let service = open_client_service(SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)?
        .context("the installed host-monitor service is missing")?;
    validate_client_service(&service, paths)?;
    ensure!(
        query_service_sid_type(&service)? == SERVICE_SID_TYPE_UNRESTRICTED,
        "the installed host-monitor service SID type is not unrestricted"
    );
    ensure!(
        query_failure_actions_on_non_crash(&service)?,
        "the installed host-monitor service does not restart after reported failures"
    );
    if validate_optional_root(&paths.state_root, "state root")? {
        validate_tree(&paths.state_root)?;
        validate_state_marker(paths, true)?;
    }
    let service_was_running = service_is_active(&service)?;
    create_protected_journal(&paths.uninstall_journal_root)?;
    let snapshot = UninstallSnapshot {
        format: SNAPSHOT_FORMAT,
        application_version: env!("CARGO_PKG_VERSION").to_owned(),
        state_acl_saved: false,
        service_was_running,
    };
    if let Err(error) = write_new_private(
        &paths.uninstall_journal_root.join(SNAPSHOT_FILE),
        &serde_json::to_vec_pretty(&snapshot)?,
    ) {
        let _ = remove_tree_no_reparse(&paths.uninstall_journal_root);
        return Err(error);
    }
    Ok(())
}

fn rollback_uninstall_preflight(paths: &FixedPaths) -> anyhow::Result<()> {
    if !rollback_path_exists(&paths.uninstall_journal_root, "uninstall journal")? {
        return Ok(());
    }
    let snapshot = read_uninstall_snapshot(paths)?;
    if snapshot.state_acl_saved || purge_started(paths)? {
        // A later, operation-specific rollback owns this journal. If that rollback failed,
        // retain the protected evidence and state needed for an administrator to recover.
        return Ok(());
    }
    remove_tree_no_reparse(&paths.uninstall_journal_root)
}

fn preserve_state(paths: &FixedPaths) -> anyhow::Result<()> {
    if !paths.state_root.exists() {
        return Ok(());
    }
    let mut snapshot = read_uninstall_snapshot(paths)?;
    validate_real_directory(&paths.state_root, "state root")?;
    validate_tree(&paths.state_root)?;
    let service = open_client_service(SERVICE_QUERY_CONFIG)?
        .context("host-monitor service disappeared during uninstall")?;
    validate_client_service(&service, paths)?;
    validate_state_marker(paths, true)?;

    let result = (|| {
        save_acl(
            &paths.state_root,
            &paths.uninstall_journal_root.join(STATE_ACL_FILE),
        )?;
        snapshot.state_acl_saved = true;
        replace_private(
            &paths.uninstall_journal_root.join(SNAPSHOT_FILE),
            &serde_json::to_vec_pretty(&snapshot)?,
        )?;
        secure_system_admin_only(&paths.state_root, true)?;
        validate_state_marker(paths, false)
    })();
    if let Err(error) = result {
        let acl_snapshot = paths.uninstall_journal_root.join(STATE_ACL_FILE);
        let rollback =
            rollback_path_exists(&acl_snapshot, "state ACL snapshot").and_then(|snapshot_exists| {
                if snapshot_exists {
                    restore_acl(&paths.state_root, &acl_snapshot)
                } else {
                    Ok(())
                }
            });
        if rollback.is_ok() {
            let _ = remove_tree_no_reparse(&paths.uninstall_journal_root);
        }
        return Err(match rollback {
            Ok(()) => error.context("state preservation failed; the original ACL was restored"),
            Err(rollback_error) => error.context(format!(
                "state preservation and ACL rollback failed; the protected uninstall journal was retained: {rollback_error:#}"
            )),
        });
    }
    Ok(())
}

fn rollback_uninstall(paths: &FixedPaths) -> anyhow::Result<()> {
    if !rollback_path_exists(&paths.uninstall_journal_root, "uninstall journal")? {
        return Ok(());
    }
    let snapshot = read_uninstall_snapshot(paths)?;
    if snapshot.state_acl_saved && rollback_path_exists(&paths.state_root, "state root")? {
        validate_tree(&paths.state_root)?;
        restore_acl(
            &paths.state_root,
            &paths.uninstall_journal_root.join(STATE_ACL_FILE),
        )?;
    }
    restore_service_state(
        paths,
        SERVICE_SID_TYPE_UNRESTRICTED,
        true,
        snapshot.service_was_running,
        ProgramAclRestore::SecureCurrent,
    )?;
    remove_tree_no_reparse(&paths.uninstall_journal_root)
}

fn commit_uninstall(paths: &FixedPaths) -> anyhow::Result<()> {
    if !paths.uninstall_journal_root.exists() {
        return Ok(());
    }
    read_uninstall_snapshot(paths)?;
    remove_tree_no_reparse(&paths.uninstall_journal_root)
}

fn prepare_purge(paths: &FixedPaths) -> anyhow::Result<()> {
    read_uninstall_snapshot(paths)?;
    ensure_absent(&paths.quarantine_root, "pending purge quarantine")?;
    if !paths.state_root.exists() {
        return Ok(());
    }
    validate_real_directory(&paths.state_root, "state root")?;
    validate_tree(&paths.state_root)?;
    let service = open_client_service(SERVICE_QUERY_CONFIG)?
        .context("host-monitor service disappeared during purge")?;
    validate_client_service(&service, paths)?;
    validate_state_marker(paths, true)?;
    write_new_private(
        &paths.uninstall_journal_root.join(PURGE_STARTED_FILE),
        PURGE_STARTED_CONTENT.as_bytes(),
    )?;
    rename_managed_directory_by_handle(
        &paths.state_root,
        &paths.quarantine_root,
        "purge quarantine",
    )?;
    Ok(())
}

fn rollback_purge(paths: &FixedPaths) -> anyhow::Result<()> {
    if !rollback_path_exists(&paths.uninstall_journal_root, "uninstall journal")? {
        return Ok(());
    }
    let snapshot = read_uninstall_snapshot(paths)?;
    if rollback_path_exists(&paths.quarantine_root, "purge quarantine")? {
        validate_real_directory(&paths.quarantine_root, "purge quarantine")?;
        validate_tree(&paths.quarantine_root)?;
        ensure_absent(&paths.state_root, "replacement state root")?;
        rename_managed_directory_by_handle(
            &paths.quarantine_root,
            &paths.state_root,
            "restored Client state root",
        )
        .context("failed to restore quarantined Client state")?;
    }
    restore_service_state(
        paths,
        SERVICE_SID_TYPE_UNRESTRICTED,
        true,
        snapshot.service_was_running,
        ProgramAclRestore::SecureCurrent,
    )?;
    remove_tree_no_reparse(&paths.uninstall_journal_root)
}

fn commit_purge(paths: &FixedPaths) -> anyhow::Result<()> {
    let journal_existed = paths.uninstall_journal_root.exists();
    if journal_existed {
        read_uninstall_snapshot(paths)?;
    }
    let deletion = if paths.quarantine_root.exists() {
        validate_real_directory(&paths.quarantine_root, "purge quarantine")
            .and_then(|_| validate_tree(&paths.quarantine_root))
            .and_then(|_| remove_tree_no_reparse(&paths.quarantine_root))
            .with_context(|| {
                format!(
                    "purge was committed but protected quarantine remains at {}; reboot and remove that fixed directory as Administrator",
                    paths.quarantine_root.display()
                )
            })
    } else {
        Ok(())
    };
    let journal_cleanup = if journal_existed {
        remove_tree_no_reparse(&paths.uninstall_journal_root)
    } else {
        Ok(())
    };
    deletion.and(journal_cleanup)
}
