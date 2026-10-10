//! Windows handle-relative adapter. No path-based rename or unlink is used.
//!
//! Native open/rename contracts:
//! https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntcreatefile
//! https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_rename_info
//! ACLs are protected at creation, not repaired after exposing new file bytes.

use std::{
    ffi::{OsStr, c_void},
    fs::File,
    io,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, Prefix},
    ptr::{null, null_mut},
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, HANDLE, INVALID_HANDLE_VALUE,
        LocalFree,
    },
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT, SetSecurityInfo,
        },
        CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid,
        GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
        GetTokenInformation, INHERITED_ACE, OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
        SetKernelObjectSecurity, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        CreateFileW, DELETE, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO,
        FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ID_INFO, FILE_READ_ATTRIBUTES,
        FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
        FILE_TYPE_DISK, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FileBasicInfo, FileDispositionInfo,
        FileIdInfo, FileStandardInfo, GetFileInformationByHandleEx, GetFileType, OPEN_EXISTING,
        READ_CONTROL, SYNCHRONIZE, SetFileInformationByHandle, WRITE_DAC, WRITE_OWNER,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

use super::{AccessPolicy, FileIdentity, FileSnapshot, PublishResult, Timestamp};
use crate::error::{AppError, Result};

const OBJ_CASE_INSENSITIVE: u32 = 0x40;
const OBJ_DONT_REPARSE: u32 = 0x1000;
const FILE_DIRECTORY_FILE: u32 = 1;
const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_WRITE_THROUGH: u32 = 2;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_OPEN_IF: u32 = 3;
const FILE_FS_FULL_SIZE_INFORMATION: u32 = 7;
const FILE_RENAME_INFORMATION: u32 = 10;
const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}
#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *mut UnicodeString,
    attributes: u32,
    security_descriptor: PSECURITY_DESCRIPTOR,
    security_qos: *mut c_void,
}
#[repr(C)]
#[derive(Default)]
struct IoStatusBlock {
    status: usize,
    information: usize,
}
#[repr(C)]
#[derive(Default)]
struct FullSizeInformation {
    total_units: i64,
    caller_available_units: i64,
    actual_available_units: i64,
    sectors_per_unit: u32,
    bytes_per_sector: u32,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtCreateFile(
        handle: *mut HANDLE,
        access: u32,
        attributes: *mut ObjectAttributes,
        status: *mut IoStatusBlock,
        allocation: *const i64,
        file_attributes: u32,
        share: u32,
        disposition: u32,
        options: u32,
        ea: *const c_void,
        ea_length: u32,
    ) -> i32;
    fn NtQueryVolumeInformationFile(
        handle: HANDLE,
        status: *mut IoStatusBlock,
        information: *mut c_void,
        length: u32,
        class: u32,
    ) -> i32;
    fn NtSetInformationFile(
        handle: HANDLE,
        status: *mut IoStatusBlock,
        information: *const c_void,
        length: u32,
        class: u32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
    fn NtFlushBuffersFileEx(
        handle: HANDLE,
        flags: u32,
        parameters: *const c_void,
        parameters_size: u32,
        status: *mut IoStatusBlock,
    ) -> i32;
}

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the allocation came from a Windows API requiring LocalFree.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

struct UserSid {
    storage: Vec<usize>,
}
impl UserSid {
    fn current() -> Result<Self> {
        let mut token = null_mut();
        // SAFETY: output handle is initialized; token rights only permit querying.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let result = (|| {
            let mut bytes = 0;
            // SAFETY: a null buffer requests the required buffer length.
            unsafe {
                GetTokenInformation(token, TokenUser, null_mut(), 0, &mut bytes);
            }
            if bytes < size_of::<TOKEN_USER>() as u32 || bytes > 65_536 {
                return Err(AppError::Unsupported(
                    "Cannot query the Windows user SID".into(),
                ));
            }
            let mut storage = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
            // SAFETY: aligned allocation has at least the requested bytes.
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    storage.as_mut_ptr().cast(),
                    bytes,
                    &mut bytes,
                )
            } == 0
            {
                return Err(io::Error::last_os_error().into());
            }
            Ok(Self { storage })
        })();
        // SAFETY: OpenProcessToken returned an owned valid handle.
        unsafe {
            CloseHandle(token);
        }
        result
    }
    fn as_ptr(&self) -> PSID {
        // SAFETY: storage was populated as TOKEN_USER and remains alive.
        unsafe { (*self.storage.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

fn private_descriptor(read_only: bool) -> Result<LocalAllocation> {
    private_descriptor_with_inheritance(read_only, false)
}

fn private_descriptor_with_inheritance(read_only: bool, inherit: bool) -> Result<LocalAllocation> {
    let sid = UserSid::current()?;
    let mut text = null_mut();
    // SAFETY: SID storage is alive, output uses the documented allocated-string API.
    if unsafe { ConvertSidToStringSidW(sid.as_ptr(), &mut text) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let allocated = LocalAllocation(text.cast());
    let mut length = 0;
    // SAFETY: ConvertSidToStringSidW returns a nul-terminated string.
    unsafe {
        while *text.add(length) != 0 {
            length += 1;
        }
    }
    let sid_text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    let access = if read_only { "FR" } else { "FA" };
    let flags = if inherit { "OICI" } else { "" };
    let sddl: Vec<u16> = format!("O:{sid_text}D:P(A;{flags};{access};;;{sid_text})")
        .encode_utf16()
        .chain([0])
        .collect();
    drop(allocated);
    let mut descriptor = null_mut();
    // SAFETY: input is nul-terminated, result is freed using LocalFree.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(LocalAllocation(descriptor))
}

fn nt_result(status: i32) -> Result<()> {
    if status < 0 {
        // SAFETY: converts the returned NT status, with no pointers.
        Err(io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32).into())
    } else {
        Ok(())
    }
}

fn open_native(
    parent: HANDLE,
    name: &OsStr,
    directory: bool,
    disposition: u32,
    access: u32,
    share: u32,
    descriptor: Option<&LocalAllocation>,
) -> Result<File> {
    let mut wide: Vec<u16> = name.encode_wide().collect();
    let length = wide
        .len()
        .checked_mul(2)
        .and_then(|size| u16::try_from(size).ok())
        .ok_or_else(|| AppError::InvalidInput("Windows file name is too long".into()))?;
    if wide.contains(&0) || length == 0 {
        return Err(AppError::InvalidInput("Unsafe Windows file name".into()));
    }
    let mut unicode = UnicodeString {
        length,
        maximum_length: length,
        buffer: wide.as_mut_ptr(),
    };
    let mut attributes = ObjectAttributes {
        length: size_of::<ObjectAttributes>() as u32,
        root_directory: parent,
        object_name: &mut unicode,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: descriptor.map_or(null_mut(), |sd| sd.0),
        security_qos: null_mut(),
    };
    let mut status = IoStatusBlock::default();
    let mut handle = null_mut();
    let options = FILE_SYNCHRONOUS_IO_NONALERT
        | if disposition != FILE_OPEN {
            FILE_WRITE_THROUGH
        } else {
            0
        }
        | FILE_OPEN_REPARSE_POINT
        | if directory {
            FILE_DIRECTORY_FILE
        } else {
            FILE_NON_DIRECTORY_FILE
        };
    // SAFETY: all buffers live through the synchronous call; RootDirectory is a borrowed handle.
    nt_result(unsafe {
        NtCreateFile(
            &mut handle,
            access | SYNCHRONIZE,
            &mut attributes,
            &mut status,
            null(),
            FILE_ATTRIBUTE_NORMAL,
            share,
            disposition,
            options,
            null(),
            0,
        )
    })?;
    // SAFETY: successful NtCreateFile transfers one owned handle to File.
    let file = unsafe { File::from_raw_handle(handle) };
    verify_kind(&file, directory)?;
    Ok(file)
}

fn info<T: Default>(file: &File, class: i32) -> Result<T> {
    let mut value = T::default();
    // SAFETY: each caller pairs the exact documented information structure and class.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            (&mut value as *mut T).cast(),
            size_of::<T>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(value)
}

fn verify_kind(file: &File, directory: bool) -> Result<()> {
    let basic: FILE_BASIC_INFO = info(file, FileBasicInfo)?;
    // SAFETY: valid borrowed file handle, no output buffer.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK
        || basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (basic.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
    {
        return Err(AppError::InvalidInput(
            "Links and non-regular Windows objects are forbidden".into(),
        ));
    }
    Ok(())
}

/// Resolve only a validated drive/UNC root through the DOS namespace. Applying
/// OBJ_DONT_REPARSE here rejects the drive alias itself on Windows 10. Every
/// caller-supplied filesystem component is opened separately by open_native.
fn open_root(root: &OsStr) -> Result<File> {
    let wide: Vec<u16> = root.encode_wide().chain([0]).collect();
    // SAFETY: root is constructed from a validated path prefix only, the
    // nul-terminated buffer lives through the call, and no creation is allowed.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: successful CreateFileW transfers one owned handle to File.
    let file = unsafe { File::from_raw_handle(handle) };
    verify_kind(&file, true)?;
    identity(&file)?;
    Ok(file)
}

pub(super) fn open_dir(path: &Path, create: bool, policy: AccessPolicy) -> Result<File> {
    let mut components = path.components();
    let root = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                format!("\\\\?\\{}:\\", drive as char)
            }
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                super::validate_component(server)?;
                super::validate_component(share)?;
                format!(
                    "\\\\?\\UNC\\{}\\{}\\",
                    server.to_string_lossy(),
                    share.to_string_lossy()
                )
            }
            _ => {
                return Err(AppError::InvalidInput(
                    "Windows device namespaces are forbidden".into(),
                ));
            }
        },
        _ => {
            return Err(AppError::InvalidInput(
                "An absolute Windows path is required".into(),
            ));
        }
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(AppError::InvalidInput(
            "Drive-relative paths are forbidden".into(),
        ));
    }
    // Pin each ancestor against rename/deletion; never change its permissions.
    let mut directory = open_root(OsStr::new(&root))?;
    let components: Vec<_> = components
        .filter(|part| !matches!(part, Component::CurDir))
        .collect();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Normal(name) => {
                if index + 1 == components.len() {
                    directory = child(&directory, name, create, policy)?;
                } else {
                    directory = match child(&directory, name, false, policy) {
                        Ok(file) => file,
                        Err(AppError::Io(error))
                            if create && error.kind() == io::ErrorKind::NotFound =>
                        {
                            child(&directory, name, true, policy)?
                        }
                        Err(error) => return Err(error),
                    };
                }
            }
            _ => {
                return Err(AppError::InvalidInput(
                    "Unsafe Windows path component".into(),
                ));
            }
        }
    }
    Ok(directory)
}

pub(super) fn child(
    parent: &File,
    name: &OsStr,
    create: bool,
    policy: AccessPolicy,
) -> Result<File> {
    super::validate_component(name)?;
    verify_kind(parent, true)?;
    let descriptor = if create && policy == AccessPolicy::Private {
        Some(private_descriptor(false)?)
    } else {
        None
    };
    // Opening an existing directory does not apply the creation security descriptor.
    let access = if create {
        FILE_GENERIC_READ
            | FILE_GENERIC_WRITE
            | if policy == AccessPolicy::Private {
                WRITE_DAC | WRITE_OWNER
            } else {
                0
            }
    } else {
        FILE_GENERIC_READ
    };
    open_native(
        parent.as_raw_handle(),
        name,
        true,
        if create { FILE_OPEN_IF } else { FILE_OPEN },
        access,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        descriptor.as_ref(),
    )
}

pub(super) fn open_regular(parent: &File, name: &OsStr) -> Result<File> {
    super::validate_component(name)?;
    verify_kind(parent, true)?;
    open_native(
        parent.as_raw_handle(),
        name,
        false,
        FILE_OPEN,
        FILE_GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        None,
    )
}

/// Pin only the principal SQLite file. SQLite must remain free to delete WAL/SHM.
pub(super) fn open_pinned_regular(parent: &File, name: &OsStr) -> Result<File> {
    super::validate_component(name)?;
    verify_kind(parent, true)?;
    open_native(
        parent.as_raw_handle(),
        name,
        false,
        FILE_OPEN,
        FILE_GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
    )
}

pub(super) fn create_new(parent: &File, name: &OsStr, policy: AccessPolicy) -> Result<File> {
    super::validate_component(name)?;
    verify_kind(parent, true)?;
    let descriptor = if policy == AccessPolicy::Private {
        Some(private_descriptor(false)?)
    } else {
        None
    };
    // DELETE access supports publication by this handle; others cannot rename/delete the stage.
    open_native(
        parent.as_raw_handle(),
        name,
        false,
        FILE_CREATE,
        FILE_GENERIC_READ
            | FILE_GENERIC_WRITE
            | DELETE
            | if policy == AccessPolicy::Private {
                WRITE_DAC | WRITE_OWNER
            } else {
                0
            },
        FILE_SHARE_READ,
        descriptor.as_ref(),
    )
}

pub(super) fn identity(file: &File) -> Result<FileIdentity> {
    let value: FILE_ID_INFO = info(file, FileIdInfo)?;
    Ok(FileIdentity {
        volume: value.VolumeSerialNumber,
        id: value.FileId.Identifier,
    })
}

fn timestamp(ticks: i64) -> Result<Timestamp> {
    let unix = ticks.checked_sub(UNIX_EPOCH_TICKS).ok_or_else(|| {
        AppError::Unsupported("Windows timestamp is outside supported bounds".into())
    })?;
    Ok(Timestamp {
        seconds: unix.div_euclid(10_000_000),
        nanos: (unix.rem_euclid(10_000_000) * 100) as u32,
    })
}

pub(super) fn snapshot(file: &File) -> Result<FileSnapshot> {
    let basic: FILE_BASIC_INFO = info(file, FileBasicInfo)?;
    let standard: FILE_STANDARD_INFO = info(file, FileStandardInfo)?;
    if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || standard.EndOfFile < 0 {
        return Err(AppError::InvalidInput(
            "Cannot snapshot a reparse point or invalid file".into(),
        ));
    }
    Ok(FileSnapshot {
        identity: identity(file)?,
        size: standard.EndOfFile as u64,
        modified: timestamp(basic.LastWriteTime)?,
        changed: timestamp(basic.ChangeTime)?,
    })
}

pub(super) fn publish_noreplace(
    parent: &File,
    staged: &File,
    staging_name: &OsStr,
    target: &OsStr,
) -> Result<PublishResult> {
    super::validate_component(staging_name)?;
    super::validate_component(target)?;
    verify_kind(parent, true)?;
    verify_kind(staged, false)?;
    // The stage handle from create_new denies FILE_SHARE_DELETE, pinning its directory entry.
    let current = open_regular(parent, staging_name)?;
    if identity(&current)? != identity(staged)? {
        return Err(AppError::Conflict(
            "Staging entry was replaced before publication".into(),
        ));
    }
    drop(current);
    staged.sync_all()?;
    let wide: Vec<u16> = target.encode_wide().collect();
    // Follow the native contract's minimum allocation, including the structure
    // and the complete name even though FileName already occupies its tail.
    let bytes = size_of::<FILE_RENAME_INFO>() + wide.len() * 2;
    // usize storage provides proper alignment for the variable-length native structure.
    let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    let rename = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: aligned zeroed buffer has the full variable name, not only FileName[1].
    unsafe {
        (*rename).Anonymous.ReplaceIfExists = false;
        (*rename).RootDirectory = parent.as_raw_handle();
        (*rename).FileNameLength = (wide.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(wide.as_ptr(), (*rename).FileName.as_mut_ptr(), wide.len());
        // The native structure has the same layout as FILE_RENAME_INFO. The
        // Win32 wrapper interprets names through the DOS namespace; use the NT
        // contract directly so RootDirectory and the relative name stay paired.
        // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information
        let mut status = IoStatusBlock::default();
        let result = NtSetInformationFile(
            staged.as_raw_handle(),
            &mut status,
            rename.cast(),
            bytes as u32,
            FILE_RENAME_INFORMATION,
        );
        if result < 0 {
            let error = io::Error::from_raw_os_error(RtlNtStatusToDosError(result) as i32);
            if matches!(error.raw_os_error(), Some(code) if code == ERROR_ALREADY_EXISTS as i32 || code == ERROR_FILE_EXISTS as i32)
            {
                return Ok(PublishResult::AlreadyExists);
            }
            return Err(error.into());
        }
    }
    // Flush the renamed file's data and metadata, in addition to directory synchronization.
    staged.sync_all()?;
    Ok(PublishResult::Published)
}

pub(super) fn remove_if_identity(
    parent: &File,
    name: &OsStr,
    expected: FileIdentity,
) -> Result<bool> {
    super::validate_component(name)?;
    verify_kind(parent, true)?;
    let open_delete = || {
        open_native(
            parent.as_raw_handle(),
            name,
            false,
            FILE_OPEN,
            FILE_READ_ATTRIBUTES | DELETE | READ_CONTROL | FILE_WRITE_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
        )
    };
    let opened = match open_delete() {
        Err(AppError::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied => {
            // Immutable private stages deny DELETE in their DACL. Only the exact
            // current-user object may have that DACL restored for cleanup.
            let read = match open_regular(parent, name) {
                Err(AppError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok(false);
                }
                result => result?,
            };
            if identity(&read)? != expected {
                return Ok(false);
            }
            if !is_private_read_only(&read)? {
                return Err(error.into());
            }
            let owner = open_native(
                parent.as_raw_handle(),
                name,
                false,
                FILE_OPEN,
                FILE_GENERIC_READ | WRITE_DAC,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
            )?;
            if identity(&owner)? != expected {
                return Ok(false);
            }
            if !is_private_read_only(&owner)? {
                return Err(error.into());
            }
            let descriptor = private_descriptor(false)?;
            // SAFETY: the owner handle pins the identity, has WRITE_DAC, and the
            // validated descriptor grants access only to the current user.
            if unsafe {
                SetKernelObjectSecurity(
                    owner.as_raw_handle(),
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    descriptor.0,
                )
            } == 0
            {
                return Err(io::Error::last_os_error().into());
            }
            drop(owner);
            drop(read);
            // Recheck identity after acquiring DELETE; never mutate a replacement.
            open_delete()
        }
        result => result,
    };
    let file = match opened {
        Ok(file) => file,
        Err(AppError::Io(error)) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if identity(&file)? != expected {
        return Ok(false);
    }
    let mut basic: FILE_BASIC_INFO = info(&file, FileBasicInfo)?;
    if basic.FileAttributes & FILE_ATTRIBUTE_READONLY != 0 {
        basic.FileAttributes &= !FILE_ATTRIBUTE_READONLY;
        if basic.FileAttributes == 0 {
            basic.FileAttributes = FILE_ATTRIBUTE_NORMAL;
        }
        basic.CreationTime = 0;
        basic.LastAccessTime = 0;
        basic.LastWriteTime = 0;
        basic.ChangeTime = 0;
        // SAFETY: the DELETE handle pins the already matched identity, and has
        // FILE_WRITE_ATTRIBUTES. Clearing readonly never targets an unchecked path.
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    let mut disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: deletion refers to this pinned object, never a fresh pathname lookup.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&mut disposition as *mut FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(true)
}

pub(super) fn free_space(parent: &File) -> Result<u64> {
    verify_kind(parent, true)?;
    let mut information = FullSizeInformation::default();
    let mut status = IoStatusBlock::default();
    // SAFETY: synchronous handle and correctly aligned documented information buffer.
    nt_result(unsafe {
        NtQueryVolumeInformationFile(
            parent.as_raw_handle(),
            &mut status,
            (&mut information as *mut FullSizeInformation).cast(),
            size_of::<FullSizeInformation>() as u32,
            FILE_FS_FULL_SIZE_INFORMATION,
        )
    })?;
    let units = u64::try_from(information.caller_available_units)
        .map_err(|_| AppError::Unsupported("Invalid Windows free-space report".into()))?;
    units
        .checked_mul(u64::from(information.sectors_per_unit))
        .and_then(|size| size.checked_mul(u64::from(information.bytes_per_sector)))
        .ok_or_else(|| {
            AppError::Unsupported("Windows free-space report exceeds supported bounds".into())
        })
}

pub(super) fn make_private(file: &File, read_only: bool) -> Result<()> {
    let descriptor = private_descriptor(read_only)?;
    set_private_descriptor(file, &descriptor)?;
    let mut basic: FILE_BASIC_INFO = info(file, FileBasicInfo)?;
    let directory = basic.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    if !directory {
        basic.FileAttributes = if read_only {
            basic.FileAttributes | FILE_ATTRIBUTE_READONLY
        } else {
            basic.FileAttributes & !FILE_ATTRIBUTE_READONLY
        };
        // Zero time fields mean preserve them, rather than restoring a stale snapshot.
        basic.CreationTime = 0;
        basic.LastAccessTime = 0;
        basic.LastWriteTime = 0;
        basic.ChangeTime = 0;
        if basic.FileAttributes == 0 {
            basic.FileAttributes = FILE_ATTRIBUTE_NORMAL;
        }
        // SAFETY: exact FileBasicInfo structure, writable-attribute handle from creation.
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    Ok(())
}

/// Owner-only inheritance permits SQLite to create private sidecars itself.
/// Existing children with broader explicit ACEs are not accepted or repaired.
pub(super) fn make_private_inheritable(directory: &File) -> Result<()> {
    verify_kind(directory, true)?;
    let descriptor = private_descriptor_with_inheritance(false, true)?;
    set_private_descriptor(directory, &descriptor)?;
    if !is_private_inheritable(directory)? {
        return Err(AppError::Conflict(
            "Private SQLite directory ACL was not established".into(),
        ));
    }
    Ok(())
}

fn set_private_descriptor(file: &File, descriptor: &LocalAllocation) -> Result<()> {
    let mut owner = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut defaulted = 0;
    let mut present = 0;
    // SAFETY: descriptor comes from the SDDL parser and remains alive through SetSecurityInfo.
    unsafe {
        if GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted) == 0
            || GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) == 0
            || present == 0
            || dacl.is_null()
        {
            return Err(io::Error::last_os_error().into());
        }
        let status = SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            owner,
            null_mut(),
            dacl,
            null(),
        );
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32).into());
        }
    }
    Ok(())
}

pub(super) fn is_private_read_only(file: &File) -> Result<bool> {
    private_access_matches(file, PrivateAccess::ReadOnly)
}

pub(super) fn is_private_read_write(file: &File) -> Result<bool> {
    private_access_matches(file, PrivateAccess::ReadWrite)
}

pub(super) fn is_private_sqlite_file(file: &File) -> Result<bool> {
    private_access_matches(file, PrivateAccess::SqliteFile)
}

pub(super) fn is_private_inheritable(directory: &File) -> Result<bool> {
    private_access_matches(directory, PrivateAccess::SqliteDirectory)
}

#[derive(Clone, Copy)]
enum PrivateAccess {
    ReadOnly,
    ReadWrite,
    SqliteFile,
    SqliteDirectory,
}

fn private_access_matches(file: &File, policy: PrivateAccess) -> Result<bool> {
    let directory = matches!(policy, PrivateAccess::SqliteDirectory);
    let read_only = matches!(policy, PrivateAccess::ReadOnly);
    verify_kind(file, directory)?;
    let basic: FILE_BASIC_INFO = info(file, FileBasicInfo)?;
    if !directory && (basic.FileAttributes & FILE_ATTRIBUTE_READONLY != 0) != read_only {
        return Ok(false);
    }
    let user = UserSid::current()?;
    let mut owner = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: GetSecurityInfo allocates descriptor and returns pointers into it.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32).into());
    }
    let _allocated = LocalAllocation(descriptor);
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: owner, ACL and ACE pointers are from the validated descriptor APIs.
    unsafe {
        if owner.is_null()
            || dacl.is_null()
            || EqualSid(owner, user.as_ptr()) == 0
            || GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) == 0
            || (*dacl).AceCount != 1
        {
            return Ok(false);
        }
        let mut ace = null_mut();
        if GetAce(dacl, 0, &mut ace) == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let header = &*ace.cast::<ACE_HEADER>();
        // Effective ACEs inherited by ordinary files carry INHERITED_ACE;
        // their DACL is not necessarily protected. The SQLite caller must
        // independently verify its pinned parent with is_private_inheritable.
        // https://learn.microsoft.com/en-us/windows/win32/secauthz/ace-inheritance-rules
        let valid_flags = match policy {
            PrivateAccess::SqliteFile if header.AceFlags == INHERITED_ACE as u8 => true,
            PrivateAccess::SqliteDirectory => {
                header.AceFlags == (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8
                    && control & SE_DACL_PROTECTED != 0
            }
            _ => header.AceFlags == 0 && control & SE_DACL_PROTECTED != 0,
        };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE
            || !valid_flags
            || usize::from(header.AceSize)
                != offset_of!(ACCESS_ALLOWED_ACE, SidStart) + GetLengthSid(user.as_ptr()) as usize
        {
            return Ok(false);
        }
        let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
        Ok(allowed.Header.AceType == ACCESS_ALLOWED_ACE_TYPE
            && allowed.Mask
                == if read_only {
                    FILE_GENERIC_READ
                } else {
                    FILE_ALL_ACCESS
                }
            && EqualSid(
                (&allowed.SidStart as *const u32).cast_mut().cast(),
                user.as_ptr(),
            ) != 0)
    }
}

pub(super) fn sync_directory(parent: &File) -> Result<()> {
    verify_kind(parent, true)?;
    // An empty native name opens the already pinned directory itself. Keep this
    // separate from open_native: caller-supplied components must never be empty.
    // ReOpenFile is a Win32 file wrapper and does not establish this directory
    // contract. Microsoft's filesystem sample handles relative NULL-name opens:
    // https://github.com/microsoft/Windows-driver-samples/blob/main/filesys/fastfat/create.c
    let mut empty = UnicodeString {
        length: 0,
        maximum_length: 0,
        buffer: null_mut(),
    };
    let mut attributes = ObjectAttributes {
        length: size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle(),
        object_name: &mut empty,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: null_mut(),
        security_qos: null_mut(),
    };
    let mut handle = null_mut();
    let mut open_status = IoStatusBlock::default();
    // SAFETY: parent is a live verified directory. All argument buffers remain
    // alive; FILE_OPEN cannot create or alter its ACL, and no DELETE is requested.
    let result = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &mut attributes,
            &mut open_status,
            null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            null(),
            0,
        )
    };
    if result < 0 {
        // SAFETY: conversion of the returned scalar status has no pointer requirements.
        let code = unsafe { RtlNtStatusToDosError(result) };
        return Err(AppError::Unsupported(format!(
            "Windows cannot obtain directory flush rights (OS error {})",
            code
        )));
    }
    // SAFETY: successful NtCreateFile transfers one owned handle, distinct from parent.
    let writable = unsafe { File::from_raw_handle(handle) };
    verify_kind(&writable, true)?;
    if identity(&writable)? != identity(parent)? {
        return Err(AppError::Conflict(
            "Directory identity changed before synchronization".into(),
        ));
    }
    let mut status = IoStatusBlock::default();
    // Flags=0 requests data, metadata and the underlying storage cache barrier.
    // DATA_SYNC_ONLY excludes directory handles; normal flushing does not.
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntflushbuffersfileex
    // SAFETY: the borrowed synchronous handle and status buffer remain alive through the call.
    let result =
        unsafe { NtFlushBuffersFileEx(writable.as_raw_handle(), 0, null(), 0, &mut status) };
    if result < 0 {
        // SAFETY: conversion of the returned scalar status has no pointer requirements.
        let code = unsafe { RtlNtStatusToDosError(result) };
        return Err(AppError::Unsupported(format!(
            "Windows cannot flush this directory (OS error {})",
            code
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn current_drive_root_opens_through_its_dos_alias() {
        use std::os::windows::fs::OpenOptionsExt;

        let current = std::env::current_dir().unwrap();
        let drive = match current.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
                _ => panic!("The native suite must run on a local drive"),
            },
            _ => panic!("The native suite must have an absolute current directory"),
        };
        let root = format!("{}:\\", drive as char);
        let expected = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .unwrap();
        let directory = open_dir(Path::new(&root), false, AccessPolicy::Shared).unwrap();
        assert_eq!(identity(&directory).unwrap(), identity(&expected).unwrap());
        let verbatim_root = format!("\\\\?\\{}:\\", drive as char);
        let verbatim = open_dir(Path::new(&verbatim_root), false, AccessPolicy::Shared).unwrap();
        assert_eq!(identity(&verbatim).unwrap(), identity(&expected).unwrap());
    }

    #[test]
    fn windows_time_uses_real_epoch_and_retains_subsecond_precision() {
        assert_eq!(
            timestamp(UNIX_EPOCH_TICKS + 12_345_678).unwrap(),
            Timestamp {
                seconds: 1,
                nanos: 234_567_800
            }
        );
        assert_eq!(
            timestamp(UNIX_EPOCH_TICKS - 1).unwrap(),
            Timestamp {
                seconds: -1,
                nanos: 999_999_900
            }
        );
    }

    #[test]
    fn exclusive_creation_publication_and_identity_cleanup_preserve_existing_bytes() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = open_dir(temporary.path(), false, AccessPolicy::Shared).unwrap();
        let mut first =
            create_new(&directory, OsStr::new("first.stage"), AccessPolicy::Private).unwrap();
        first.write_all(b"original bytes").unwrap();
        assert!(create_new(&directory, OsStr::new("first.stage"), AccessPolicy::Private).is_err());
        assert_eq!(
            publish_noreplace(
                &directory,
                &first,
                OsStr::new("first.stage"),
                OsStr::new("published")
            )
            .unwrap(),
            PublishResult::Published
        );
        let mut second = create_new(
            &directory,
            OsStr::new("second.stage"),
            AccessPolicy::Private,
        )
        .unwrap();
        second.write_all(b"replacement bytes").unwrap();
        assert_eq!(
            publish_noreplace(
                &directory,
                &second,
                OsStr::new("second.stage"),
                OsStr::new("published")
            )
            .unwrap(),
            PublishResult::AlreadyExists
        );
        let expected = identity(&first).unwrap();
        let other = identity(&second).unwrap();
        drop(first);
        drop(second);
        assert!(!remove_if_identity(&directory, OsStr::new("published"), other).unwrap());
        assert_eq!(
            std::fs::read(temporary.path().join("published")).unwrap(),
            b"original bytes"
        );
        assert!(remove_if_identity(&directory, OsStr::new("published"), expected).unwrap());
        assert!(!remove_if_identity(&directory, OsStr::new("published"), expected).unwrap());
    }

    #[test]
    fn private_read_only_policy_checks_the_acl_not_only_the_attribute() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = open_dir(temporary.path(), false, AccessPolicy::Shared).unwrap();
        let mut file =
            create_new(&directory, OsStr::new("immutable"), AccessPolicy::Private).unwrap();
        file.write_all(b"immutable").unwrap();
        assert!(!is_private_read_only(&file).unwrap());
        assert!(is_private_read_write(&file).unwrap());
        make_private(&file, true).unwrap();
        assert!(is_private_read_only(&file).unwrap());
        assert!(!is_private_read_write(&file).unwrap());
        drop(file);
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(temporary.path().join("immutable"))
                .is_err()
        );
        // The owner retains WRITE_DAC; restore the DACL without asking for data writes.
        let file = open_native(
            directory.as_raw_handle(),
            OsStr::new("immutable"),
            false,
            FILE_OPEN,
            FILE_GENERIC_READ | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
        );
        let file = file.unwrap();
        let descriptor = private_descriptor(false).unwrap();
        // SAFETY: descriptor is a validated SDDL allocation and handle has WRITE_DAC.
        assert_ne!(
            unsafe {
                SetKernelObjectSecurity(
                    file.as_raw_handle(),
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    descriptor.0,
                )
            },
            0
        );
        drop(file);
        let file = open_native(
            directory.as_raw_handle(),
            OsStr::new("immutable"),
            false,
            FILE_OPEN,
            FILE_GENERIC_READ | WRITE_DAC | WRITE_OWNER | FILE_WRITE_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
        )
        .unwrap();
        make_private(&file, false).unwrap();
        drop(file);
        drop(directory);
        temporary.close().unwrap();
    }

    #[test]
    fn writable_policy_rejects_broadened_acl_without_repairing_it() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = open_dir(temporary.path(), false, AccessPolicy::Shared).unwrap();
        let file = create_new(
            &directory,
            OsStr::new("runtime.lock"),
            AccessPolicy::Private,
        )
        .unwrap();
        assert!(is_private_read_write(&file).unwrap());
        let sddl: Vec<u16> = "D:P(A;;FA;;;WD)".encode_utf16().chain([0]).collect();
        let mut descriptor = null_mut();
        // SAFETY: this fixed fixture descriptor contains no untrusted values; it
        // deliberately adds Everyone solely to an empty temporary test lock.
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    null_mut(),
                )
            },
            0
        );
        let descriptor = LocalAllocation(descriptor);
        // SAFETY: owned temporary handle has WRITE_DAC; descriptor is validated and alive.
        assert_ne!(
            unsafe {
                SetKernelObjectSecurity(
                    file.as_raw_handle(),
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    descriptor.0,
                )
            },
            0
        );
        let unchanged = snapshot(&file).unwrap();
        assert!(!is_private_read_write(&file).unwrap());
        unchanged.verify(&file).unwrap();
        assert!(!is_private_read_write(&file).unwrap());
        let expected = identity(&file).unwrap();
        drop(file);
        assert!(remove_if_identity(&directory, OsStr::new("runtime.lock"), expected).unwrap());
        drop(directory);
        temporary.close().unwrap();
    }

    #[test]
    fn sqlite_inherited_owner_policy_keeps_sidecars_private_and_principal_pinned() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile");
        let directory = open_dir(&path, true, AccessPolicy::Private).unwrap();
        assert!(!is_private_inheritable(&directory).unwrap());
        make_private_inheritable(&directory).unwrap();
        assert!(is_private_inheritable(&directory).unwrap());

        // Like SQLite's Win32 VFS, the OS creates these files without an
        // application-supplied descriptor, inheriting the verified parent ACL.
        let database = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path.join("library.sqlite"))
            .unwrap();
        assert!(is_private_sqlite_file(&database).unwrap());
        assert!(!is_private_read_write(&database).unwrap());
        let pinned = open_pinned_regular(&directory, OsStr::new("library.sqlite")).unwrap();
        assert!(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path.join("library.sqlite"))
                .is_ok()
        );
        assert!(
            std::fs::rename(path.join("library.sqlite"), path.join("replaced.sqlite")).is_err()
        );
        assert!(std::fs::remove_file(path.join("library.sqlite")).is_err());
        for name in ["library.sqlite-wal", "library.sqlite-shm"] {
            let sidecar = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path.join(name))
                .unwrap();
            assert!(is_private_sqlite_file(&sidecar).unwrap());
            drop(sidecar);
            // Sidecars are not pinned, so SQLite can remove and recreate them.
            std::fs::remove_file(path.join(name)).unwrap();
        }
        let explicit = create_new(
            &directory,
            OsStr::new("explicit.sqlite"),
            AccessPolicy::Private,
        )
        .unwrap();
        assert!(is_private_sqlite_file(&explicit).unwrap());
        make_private(&explicit, true).unwrap();
        assert!(!is_private_sqlite_file(&explicit).unwrap());
        make_private(&explicit, false).unwrap();
        drop(explicit);
        drop(pinned);
        drop(database);
        drop(directory);
        temporary.close().unwrap();
    }

    #[test]
    fn sqlite_policy_rejects_everyone_and_non_effective_inheritance_without_repair() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile");
        let directory = open_dir(&path, true, AccessPolicy::Private).unwrap();
        make_private_inheritable(&directory).unwrap();
        let file = create_new(
            &directory,
            OsStr::new("broad.sqlite"),
            AccessPolicy::Private,
        )
        .unwrap();
        let sddl: Vec<u16> = "D:P(A;;FA;;;WD)".encode_utf16().chain([0]).collect();
        let mut descriptor = null_mut();
        // SAFETY: fixed test-only SDDL intentionally broadens a temporary file;
        // the validated allocation is owned until the setter completes.
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    null_mut(),
                )
            },
            0
        );
        let descriptor = LocalAllocation(descriptor);
        // SAFETY: temporary owned file has WRITE_DAC and descriptor remains live.
        assert_ne!(
            unsafe {
                SetKernelObjectSecurity(
                    file.as_raw_handle(),
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    descriptor.0,
                )
            },
            0
        );
        let unchanged = snapshot(&file).unwrap();
        assert!(!is_private_sqlite_file(&file).unwrap());
        unchanged.verify(&file).unwrap();

        let descriptor = private_descriptor_with_inheritance(false, true).unwrap();
        let mut dacl = null_mut();
        let mut present = 0;
        let mut defaulted = 0;
        let mut ace = null_mut();
        // SAFETY: descriptor contains one verified ACCESS_ALLOWED_ACE. Adding
        // INHERIT_ONLY to the test allocation makes it ineffective on parent.
        unsafe {
            assert_ne!(
                GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted),
                0
            );
            assert_eq!(present, 1);
            assert_ne!(GetAce(dacl, 0, &mut ace), 0);
            (*ace.cast::<ACE_HEADER>()).AceFlags |=
                windows_sys::Win32::Security::INHERIT_ONLY_ACE as u8;
            assert_ne!(
                SetKernelObjectSecurity(
                    directory.as_raw_handle(),
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    descriptor.0
                ),
                0
            );
        }
        assert!(!is_private_inheritable(&directory).unwrap());
        make_private_inheritable(&directory).unwrap();
        assert!(is_private_inheritable(&directory).unwrap());
        drop(file);
        drop(directory);
        temporary.close().unwrap();
    }

    #[test]
    fn read_only_stage_cleanup_requires_the_exact_identity_and_keeps_other_entries_immutable() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = open_dir(temporary.path(), false, AccessPolicy::Shared).unwrap();
        let mut file = create_new(
            &directory,
            OsStr::new("immutable.stage"),
            AccessPolicy::Private,
        )
        .unwrap();
        file.write_all(b"private original").unwrap();
        make_private(&file, true).unwrap();
        let expected = identity(&file).unwrap();
        drop(file);
        let mut wrong = expected;
        wrong.id[0] ^= 1;
        assert!(!remove_if_identity(&directory, OsStr::new("immutable.stage"), wrong).unwrap());
        let checked = open_regular(&directory, OsStr::new("immutable.stage")).unwrap();
        assert!(is_private_read_only(&checked).unwrap());
        assert_eq!(
            std::fs::read(temporary.path().join("immutable.stage")).unwrap(),
            b"private original"
        );
        drop(checked);
        assert!(remove_if_identity(&directory, OsStr::new("immutable.stage"), expected).unwrap());
        assert!(!temporary.path().join("immutable.stage").exists());
        drop(directory);
        temporary.close().unwrap();
    }

    #[test]
    fn junctions_alternate_streams_and_device_namespaces_are_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("sentinel"), b"outside").unwrap();
        let junction = temporary.path().join("junction");
        let output = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "The Windows runner must support directory junction fixtures"
        );
        let directory = open_dir(temporary.path(), false, AccessPolicy::Shared).unwrap();
        assert!(
            child(
                &directory,
                OsStr::new("junction"),
                false,
                AccessPolicy::Shared
            )
            .is_err()
        );
        assert!(open_dir(&junction, false, AccessPolicy::Shared).is_err());
        assert!(open_regular(&directory, OsStr::new("sentinel:stream")).is_err());
        assert!(open_dir(Path::new(r"\\.\C:\"), false, AccessPolicy::Shared).is_err());
        assert!(open_dir(Path::new(r"C:relative"), false, AccessPolicy::Shared).is_err());
        assert!(
            remove_if_identity(
                &directory,
                OsStr::new("junction"),
                FileIdentity {
                    volume: 0,
                    id: [0; 16]
                }
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(outside.path().join("sentinel")).unwrap(),
            b"outside"
        );
        drop(directory);
        std::fs::remove_dir(junction).unwrap();
        temporary.close().unwrap();
        outside.close().unwrap();
    }

    #[test]
    fn created_directory_has_a_real_native_durability_barrier() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = open_dir(
            &temporary.path().join("durable"),
            true,
            AccessPolicy::Private,
        )
        .unwrap();
        // Native Windows execution must establish support; no skipped assertion or Ok stub.
        sync_directory(&directory).unwrap();
        drop(directory);
        let read_handle = open_dir(
            &temporary.path().join("durable"),
            false,
            AccessPolicy::Private,
        )
        .unwrap();
        // Creation permission is not required to synchronize an existing directory.
        sync_directory(&read_handle).unwrap();
    }

    #[test]
    fn relative_publication_preserves_unicode_and_single_character_names() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("destination");
        let parent = open_dir(&destination, true, AccessPolicy::Private).unwrap();
        for (stage_name, target, bytes) in [
            (
                "unicode-stage",
                "métadonnées-読み物.epub",
                b"unicode bytes".as_slice(),
            ),
            ("short-stage", "x", b"short name bytes".as_slice()),
        ] {
            let mut stage =
                create_new(&parent, OsStr::new(stage_name), AccessPolicy::Private).unwrap();
            stage.write_all(bytes).unwrap();
            let original_identity = identity(&stage).unwrap();
            assert_eq!(
                publish_noreplace(&parent, &stage, OsStr::new(stage_name), OsStr::new(target))
                    .unwrap(),
                PublishResult::Published
            );
            let published = open_regular(&parent, OsStr::new(target)).unwrap();
            assert_eq!(identity(&published).unwrap(), original_identity);
            assert_eq!(std::fs::read(destination.join(target)).unwrap(), bytes);
            assert!(!destination.join(stage_name).exists());
        }
        sync_directory(&parent).unwrap();
    }

    #[test]
    fn synchronizing_an_existing_directory_preserves_its_identity_and_private_acl() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("private");
        let created = open_dir(&path, true, AccessPolicy::Private).unwrap();
        let expected = identity(&created).unwrap();
        assert!(is_private_read_write(&created).unwrap());
        drop(created);
        let read_handle = open_dir(&path, false, AccessPolicy::Private).unwrap();
        for _ in 0..2 {
            sync_directory(&read_handle).unwrap();
            assert_eq!(identity(&read_handle).unwrap(), expected);
            assert!(is_private_read_write(&read_handle).unwrap());
        }
    }
}
