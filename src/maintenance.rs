//! Outer, bounded maintenance gate. Fixed order: maintenance, instance, state.
use std::path::Path;
pub struct Guard {
    _directory: xcsc::fs_safety::PrivateDirectory,
    _lock: xcsc::fs_safety::AdvisoryLock,
}
impl Guard {
    pub fn acquire(path: &Path) -> anyhow::Result<Self> {
        #[cfg(unix)]
        use xcsc::fs_safety::PrivateDirectory;
        use xcsc::fs_safety::{AdvisoryLock, EntryName};
        #[cfg(unix)]
        let directory = PrivateDirectory::create_for_administration(path)?;
        #[cfg(windows)]
        let directory = runtime_directory(path)?;
        let lock = AdvisoryLock::acquire(
            &directory,
            &EntryName::new("maintenance.lock")?.as_relative(),
        )?;
        Ok(Self {
            _directory: directory,
            _lock: lock,
        })
    }
}
/// Retain the validated private directory and its product service access policy.
pub fn runtime_directory(
    path: &std::path::Path,
) -> anyhow::Result<xcsc::fs_safety::PrivateDirectory> {
    runtime_directory_inner(path, true)
}
pub fn open_runtime_directory(
    path: &std::path::Path,
) -> anyhow::Result<xcsc::fs_safety::PrivateDirectory> {
    runtime_directory_inner(path, false)
}
/// SCM state always uses the installed product service role.
#[cfg(windows)]
pub fn open_service_runtime_directory(
    path: &std::path::Path,
) -> anyhow::Result<xcsc::fs_safety::PrivateDirectory> {
    let sid = xcsc::fs_safety::service_sid(crate::service::WINDOWS_SERVICE_NAME)?;
    Ok(xcsc::fs_safety::PrivateDirectory::open_with_windows_access(
        path,
        xcsc::fs_safety::WindowsPrivateAccess::for_service(&sid, "S-1-5-19")?,
    )?)
}
fn runtime_directory_inner(
    path: &std::path::Path,
    create: bool,
) -> anyhow::Result<xcsc::fs_safety::PrivateDirectory> {
    #[cfg(unix)]
    {
        Ok(if create {
            xcsc::fs_safety::PrivateDirectory::create(path)?
        } else {
            xcsc::fs_safety::PrivateDirectory::open_existing(path)?
        })
    }
    #[cfg(windows)]
    {
        let requires_service_role = matches!(
            xcsc::fs_safety::process_user_sid()?.as_str(),
            "S-1-5-19" | "S-1-5-20"
        );
        // A custom user directory is created with the shared current-user ACL.
        // SCM state created by the installer already carries the service role.
        // Do not create a new broad LocalService grant for ordinary CLI runs.
        if create && !path.try_exists()? {
            anyhow::ensure!(
                !requires_service_role,
                "built-in service accounts require installed service state"
            );
            let directory = xcsc::fs_safety::PrivateDirectory::create_with_windows_access(
                path,
                xcsc::fs_safety::WindowsPrivateAccess::for_current_user()?,
            )?;
            return Ok(directory);
        }
        {
            let sid = xcsc::fs_safety::service_sid(crate::service::WINDOWS_SERVICE_NAME)?;
            let access = xcsc::fs_safety::WindowsPrivateAccess::for_service(&sid, "S-1-5-19")?;
            match xcsc::fs_safety::PrivateDirectory::open_with_windows_access(path, access) {
                Ok(directory) => return Ok(directory),
                Err(xcsc::fs_safety::Error::UnsafePermissions(_)) if !requires_service_role => {}
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::ensure!(
            !requires_service_role,
            "built-in service accounts require the product service role"
        );
        Ok(xcsc::fs_safety::PrivateDirectory::open_with_windows_access(
            path,
            xcsc::fs_safety::WindowsPrivateAccess::for_current_user()?,
        )?)
    }
}
