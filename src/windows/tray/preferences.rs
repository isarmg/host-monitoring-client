fn ensure_fixed_regular_file(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("{label} is missing: {}", path.display()))?;
    ensure!(metadata.is_file(), "{label} is not a regular file");
    Ok(())
}

fn load_preferences(path: &Path) -> anyhow::Result<TrayPreferences> {
    let bytes = match super::read_bounded_tray_preferences_file(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TrayPreferences::default());
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(
        bytes.len() <= super::MAX_TRAY_PREFERENCES_BYTES,
        "tray preferences file is too large"
    );
    let mut preferences: TrayPreferences =
        serde_json::from_slice(&bytes).context("tray preferences are invalid")?;
    if !preferences.server.is_empty() {
        preferences.server = validate_server_base(&preferences.server)?;
    }
    Ok(preferences)
}

fn save_preferences(path: &Path, preferences: &TrayPreferences) -> anyhow::Result<()> {
    let parent = path.parent().context("tray preferences have no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".tray-{}.tmp", random_secret()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, preferences)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        let temporary_wide = wide_os(temporary.as_os_str());
        let path_wide = wide_os(path.as_os_str());
        unsafe {
            MoveFileExW(
                PCWSTR(temporary_wide.as_ptr()),
                PCWSTR(path_wide.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .context("failed to atomically replace tray preferences")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.context("failed to save per-user tray preferences")
}

fn random_secret() -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = rand::random::<[u8; 32]>();
    let mut value = String::with_capacity(64);
    for byte in bytes {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    value
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn wide_os(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn copy_wide_fixed<const LENGTH: usize>(destination: &mut [u16; LENGTH], value: &str) {
    for (destination, source) in destination
        .iter_mut()
        .zip(value.encode_utf16().take(LENGTH.saturating_sub(1)))
    {
        *destination = source;
    }
}
