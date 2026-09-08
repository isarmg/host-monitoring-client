//! Native Windows handles and ACL validation shared by maintenance and credential state.
use std::io;
type StorageError = io::Error;
fn storage_error(_: impl std::fmt::Debug) -> StorageError {
    io::Error::other("protected maintenance state unavailable")
}

use std::{
    ffi::{OsStr, c_void},
    fs::{File, OpenOptions},
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::Threading::*,
};

pub struct DirectoryGuard {
    _ancestors: Vec<File>,
    path: PathBuf,
}
fn wide(value: &OsStr) -> Result<Vec<u16>, StorageError> {
    let mut bytes: Vec<u16> = value.encode_wide().collect();
    if bytes.contains(&0) {
        return Err(storage_error(()));
    }
    bytes.push(0);
    Ok(bytes)
}
struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn sid_text(sid: PSID) -> Result<String, StorageError> {
    unsafe {
        let mut output = std::ptr::null_mut();
        if ConvertSidToStringSidW(sid, &mut output) == 0 {
            return Err(storage_error(()));
        }
        let _owned = LocalAllocation(output.cast());
        let mut len = 0;
        while *output.add(len) != 0 {
            len += 1;
            if len > 256 {
                return Err(storage_error(()));
            }
        }
        String::from_utf16(std::slice::from_raw_parts(output, len)).map_err(storage_error)
    }
}
fn current_sid() -> Result<String, StorageError> {
    unsafe {
        let mut raw = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) == 0 {
            return Err(storage_error(()));
        }
        let token = File::from_raw_handle(raw);
        let mut needed = 0;
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
        if needed == 0 || needed > 16384 {
            return Err(storage_error(()));
        }
        // u64 storage supplies native alignment for TOKEN_USER.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        ) == 0
        {
            return Err(storage_error(()));
        }
        sid_text((*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid)
    }
}
fn private_descriptor(inherit: bool) -> Result<Vec<u16>, StorageError> {
    // LocalService must be able to create its own lock/state files without
    // assigning an owner SID (Administrators) that is absent from its token.
    let owner = if current_sid()? == "S-1-5-19" {
        "LS"
    } else {
        "BA"
    };
    let flags = if inherit { "OICI" } else { "" };
    let service = service_sid().unwrap_or_else(|| "LS".into());
    wide(OsStr::new(&format!(
        "O:{owner}G:{owner}D:P(A;{flags};FA;;;SY)(A;{flags};FA;;;BA)(A;{flags};0x1301bf;;;{service})(A;{flags};RC;;;OW)"
    )))
}
fn trusted(sid: &str, current: &str) -> bool {
    sid == current
        || sid == "S-1-5-18"
        || sid == "S-1-5-32-544"
        || sid == "S-1-5-19"
        || service_sid().as_deref() == Some(sid)
}
fn verify_private(file: &File) -> Result<(), StorageError> {
    let current = current_sid()?;
    unsafe {
        let mut owner = std::ptr::null_mut();
        let mut acl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        if GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut acl,
            std::ptr::null_mut(),
            &mut descriptor,
        ) != 0
        {
            return Err(storage_error(()));
        }
        let _owned = LocalAllocation(descriptor);
        if owner.is_null() || acl.is_null() || !trusted(&sid_text(owner)?, &current) {
            return Err(storage_error(()));
        }
        if (*acl).AceCount == 0 {
            return Err(storage_error(()));
        }
        for index in 0..(*acl).AceCount {
            let mut ace = std::ptr::null_mut();
            if GetAce(acl, index.into(), &mut ace) == 0 {
                return Err(storage_error(()));
            }
            let header = &*ace.cast::<ACE_HEADER>();
            // Only simple allow ACEs for the service identity, SYSTEM and Administrators.
            // Unknown/object/callback ACEs fail closed; inheritance-only ACEs are checked too.
            if header.AceType != 0 {
                return Err(storage_error(()));
            }
            let allow = &*ace.cast::<ACCESS_ALLOWED_ACE>();
            let trustee = sid_text(std::ptr::addr_of!(allow.SidStart).cast_mut().cast())?;
            // Installer ACLs restrict the already-validated owner's implicit
            // WRITE_DAC permission using a ReadControl-only OWNER RIGHTS ACE.
            if trustee == "S-1-3-4" && allow.Mask == READ_CONTROL {
                continue;
            }
            if !trusted(&trustee, &current) {
                return Err(storage_error(()));
            }
        }
    }
    Ok(())
}
fn verify_kind(file: &File, directory: bool) -> Result<(), StorageError> {
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0
            || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
            || (!directory && info.nNumberOfLinks != 1)
        {
            return Err(storage_error(()));
        }
    }
    Ok(())
}
fn directory_handle(path: &Path) -> Result<File, StorageError> {
    let file = OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    verify_kind(&file, true)?;
    Ok(file)
}
pub(crate) fn open_root(path: &Path, create: bool) -> Result<DirectoryGuard, StorageError> {
    // Local DOS drive paths, including canonical verbatim drive paths; no UNC, device paths, ADS or traversal.
    let mut components = path.components();
    if !matches!(components.next(),Some(Component::Prefix(p)) if matches!(p.kind(),Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        || !matches!(components.next(), Some(Component::RootDir))
    {
        return Err(storage_error(()));
    }
    let parts: Vec<_> = components.collect();
    if parts.is_empty()||parts.iter().any(|c|!matches!(c,Component::Normal(n) if !n.to_string_lossy().contains([':', '*', '?'])&&!n.to_string_lossy().ends_with(['.',' ']))) {return Err(storage_error(()));}
    let parent = path.parent().ok_or_else(|| storage_error(()))?;
    let mut ancestors = Vec::new();
    let mut cursor = PathBuf::new();
    for part in parent.components() {
        cursor.push(part.as_os_str());
        if matches!(part, Component::Prefix(_)) {
            continue;
        }
        ancestors.push(directory_handle(&cursor)?);
    }
    let sddl = private_descriptor(true)?;
    unsafe {
        let mut descriptor = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(storage_error(()));
        }
        let _owned = LocalAllocation(descriptor);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        if create
            && CreateDirectoryW(wide(path.as_os_str())?.as_ptr(), &attributes) == 0
            && GetLastError() != ERROR_ALREADY_EXISTS
        {
            return Err(storage_error(()));
        }
    }
    let leaf = directory_handle(path)?;
    verify_private(&leaf)?;
    ancestors.push(leaf);
    Ok(DirectoryGuard {
        _ancestors: ancestors,
        path: path.to_owned(),
    })
}
pub(crate) fn private_file(
    path: &Path,
    create: bool,
    exclusive: bool,
) -> Result<File, StorageError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .share_mode(if exclusive {
            0
        } else {
            FILE_SHARE_READ | FILE_SHARE_WRITE
        })
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = if create {
        unsafe {
            let mut descriptor = std::ptr::null_mut();
            let sddl = private_descriptor(false)?;
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(storage_error(()));
            }
            let _owned = LocalAllocation(descriptor);
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let raw = CreateFileW(
                wide(path.as_os_str())?.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                if exclusive {
                    0
                } else {
                    FILE_SHARE_READ | FILE_SHARE_WRITE
                },
                &attributes,
                OPEN_ALWAYS,
                FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            );
            if raw == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            File::from_raw_handle(raw)
        }
    } else {
        options.open(path)?
    };
    verify_kind(&file, false)?;
    verify_private(&file)?;
    Ok(file)
}

fn service_sid() -> Option<String> {
    unsafe {
        let name = wide(OsStr::new("NT SERVICE\\host-monitor")).ok()?;
        let mut sid_len = 0;
        let mut domain_len = 0;
        let mut kind = 0;
        LookupAccountNameW(
            std::ptr::null(),
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut sid_len,
            std::ptr::null_mut(),
            &mut domain_len,
            &mut kind,
        );
        if sid_len == 0 || sid_len > 1024 || domain_len > 1024 {
            return None;
        }
        let mut sid = vec![0u64; (sid_len as usize).div_ceil(8)];
        let mut domain = vec![0u16; domain_len as usize];
        if LookupAccountNameW(
            std::ptr::null(),
            name.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        ) == 0
        {
            return None;
        }
        sid_text(sid.as_mut_ptr().cast()).ok()
    }
}
pub struct Guard {
    _directory: DirectoryGuard,
    _file: File,
}
impl Guard {
    pub fn acquire(path: &Path) -> anyhow::Result<Self> {
        Self::acquire_named(path, "maintenance.lock")
    }
    pub(crate) fn acquire_named(path: &Path, name: &str) -> anyhow::Result<Self> {
        let directory = open_root(path, true)?;
        let file = private_file(&directory.path.join(name), true, true)?;
        Ok(Self {
            _directory: directory,
            _file: file,
        })
    }
}

pub(crate) fn create_temporary(path: &Path) -> io::Result<File> {
    unsafe {
        let mut descriptor = std::ptr::null_mut();
        let sddl = private_descriptor(false)?;
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let _owned = LocalAllocation(descriptor);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let raw = CreateFileW(
            wide(path.as_os_str())?.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            &attributes,
            CREATE_NEW,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH,
            std::ptr::null_mut(),
        );
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let file = File::from_raw_handle(raw);
        verify_kind(&file, false)?;
        verify_private(&file)?;
        Ok(file)
    }
}
