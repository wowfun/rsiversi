//! Narrow synchronous Win32/NT calls. Every returned handle has one File owner.
use super::{DirectoryEntry, names};
use crate::EntryKind;
mod security;
use std::{
    ffi::{OsString, c_void},
    fs::File,
    io,
    os::windows::{
        ffi::OsStringExt as _,
        io::{AsRawHandle as _, FromRawHandle as _},
    },
};

type Handle = *mut c_void;
#[repr(C)]
struct UnicodeName {
    length: u16,
    maximum: u16,
    buffer: *const u16,
}
#[repr(C)]
struct Attributes {
    length: u32,
    root: Handle,
    name: *const UnicodeName,
    flags: u32,
    security: *const c_void,
    quality: *const c_void,
}
#[repr(C)]
#[derive(Default)]
struct StatusBlock {
    status: isize,
    information: usize,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSetInformationFile(
        file: Handle,
        status: *mut StatusBlock,
        data: *const c_void,
        size: u32,
        kind: u32,
    ) -> i32;
    fn NtCreateFile(
        file: *mut Handle,
        access: u32,
        attributes: *const Attributes,
        status: *mut StatusBlock,
        allocation: *const i64,
        file_attributes: u32,
        sharing: u32,
        disposition: u32,
        options: u32,
        ea: *const c_void,
        ea_length: u32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
    fn NtQueryDirectoryFileEx(
        file: Handle,
        event: Handle,
        apc: *const c_void,
        context: *const c_void,
        status: *mut StatusBlock,
        data: *mut c_void,
        size: u32,
        kind: u32,
        flags: u32,
        name: *const UnicodeName,
    ) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        sharing: u32,
        security: *const c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn GetVolumeInformationByHandleW(
        file: Handle,
        name: *mut u16,
        name_size: u32,
        serial: *mut u32,
        maximum_component: *mut u32,
        flags: *mut u32,
        filesystem: *mut u16,
        filesystem_size: u32,
    ) -> i32;
    fn GetFileInformationByHandleEx(file: Handle, kind: u32, data: *mut c_void, size: u32) -> i32;
    #[cfg(test)]
    fn DeviceIoControl(
        file: Handle,
        code: u32,
        input: *const c_void,
        input_size: u32,
        output: *mut c_void,
        output_size: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
}

const DIRECTORY_READ: u32 = 0x0010_0081;
const FILE_READ: u32 = 0x0012_0089;
const REPARSE: u32 = 0x400;
const DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_DEVICE: u32 = 0x40;
pub(super) const ERROR_REPARSE_POINT_ENCOUNTERED: i32 = 4395;
pub(super) const ERROR_DIRECTORY: i32 = 267;
const OBJ_CASE_INSENSITIVE: u32 = 0x40;
const OBJ_DONT_REPARSE: u32 = 0x1000;
const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_DIRECTORY_FILE: u32 = 1;
const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_ATTRIBUTE_TAG_INFO: u32 = 9;
const BUFFER_SIZE: u32 = 65536;

fn nt_result(status: i32) -> io::Result<()> {
    if status >= 0 {
        return Ok(());
    }
    // SAFETY: Pure NTSTATUS-to-Win32 conversion has no pointer or lifetime requirements.
    Err(io::Error::from_raw_os_error(
        unsafe { RtlNtStatusToDosError(status) }.cast_signed(),
    ))
}

pub(super) fn root(drive: u16) -> io::Result<File> {
    let short = [drive, 58, 92, 0];
    // SAFETY: short is a live NUL-terminated UTF-16 drive root for the whole call.
    if unsafe { GetDriveTypeW(short.as_ptr()) } != 3 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "fixed local drive required",
        ));
    }
    let name = [92, 92, 63, 92, drive, 58, 92, 0];
    // SAFETY: name is NUL-terminated; no security descriptor means a non-inheritable
    // handle. OPEN_EXISTING and OPEN_REPARSE_POINT do not create or follow the root.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            DIRECTORY_READ,
            7,
            std::ptr::null(),
            3,
            0x0220_0000,
            std::ptr::null_mut(),
        )
    };
    if handle as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a new valid handle; ownership transfers exactly once.
    let file = unsafe { File::from_raw_handle(handle) };
    require_type(&file, true)?;
    if long_name(&file)? != [92] {
        return Err(names::invalid("drive root is a directory alias"));
    }
    let mut filesystem = [0_u16; 64];
    // SAFETY: The live file handle and writable filesystem buffer remain valid during
    // this synchronous call; unused optional outputs are null.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            filesystem.as_mut_ptr(),
            64,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if filesystem[..5] != [78, 84, 70, 83, 0] {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "local NTFS required",
        ));
    }
    Ok(file)
}

pub(super) fn relative(parent: &File, name: &[u16], directory: bool) -> io::Result<File> {
    let (file, _) = acquire(parent, name, directory, None)?;
    check_opened(&file, name, directory)?;
    Ok(file)
}

fn acquire(
    parent: &File,
    name: &[u16],
    directory: bool,
    private: Option<&security::PrivateDescriptor>,
) -> io::Result<(File, usize)> {
    acquire_with_access(
        parent,
        name,
        directory,
        private,
        if directory { DIRECTORY_READ } else { FILE_READ },
        if private.is_some() {
            FILE_CREATE
        } else {
            FILE_OPEN
        },
        7,
    )
}

fn acquire_with_access(
    parent: &File,
    name: &[u16],
    directory: impl Into<Option<bool>>,
    private: Option<&security::PrivateDescriptor>,
    access: u32,
    disposition: u32,
    sharing: u32,
) -> io::Result<(File, usize)> {
    let directory = directory.into();
    let bytes =
        u16::try_from(name.len() * 2).map_err(|_| names::invalid("native name too long"))?;
    let text = UnicodeName {
        length: bytes,
        maximum: bytes,
        buffer: name.as_ptr(),
    };
    let attributes = Attributes {
        length: u32::try_from(std::mem::size_of::<Attributes>())
            .expect("native attribute record size"),
        root: parent.as_raw_handle(),
        name: &raw const text,
        flags: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security: private.map_or(std::ptr::null(), security::PrivateDescriptor::as_ptr),
        quality: std::ptr::null(),
    };
    let mut handle = std::ptr::null_mut();
    let mut io_status = StatusBlock::default();
    // SAFETY: All input records, the counted UTF-16 slice (possibly empty for an
    // internal directory reopen), output records and parent handle remain live.
    // SYNCHRONOUS_IO_NONALERT avoids pending I/O; OBJ_INHERIT is absent. The optional
    // owned descriptor stays live and is applied only by FILE_CREATE. FILE_OPEN
    // cannot mutate. OBJ_DONT_REPARSE rejects reparses during name parsing;
    // FILE_OPEN_REPARSE_POINT bypasses final-component reparse processing. The
    // owning README links both NT contracts. Payload callers use check_opened
    // to reject reparses; child_kind checks the exact name and classifies the
    // retained handle's attributes without exposing it for payload access.
    let status = unsafe {
        NtCreateFile(
            &raw mut handle,
            access | if private.is_some() { 0x20000 } else { 0 },
            &raw const attributes,
            &raw mut io_status,
            std::ptr::null(),
            0x80,
            sharing,
            disposition,
            FILE_OPEN_REPARSE_POINT
                | FILE_SYNCHRONOUS_IO_NONALERT
                | match directory {
                    Some(true) => FILE_DIRECTORY_FILE,
                    Some(false) => FILE_NON_DIRECTORY_FILE,
                    None => 0,
                },
            std::ptr::null(),
            0,
        )
    };
    nt_result(status)?;
    // SAFETY: Successful synchronous NtCreateFile returned one new handle.
    let file = unsafe { File::from_raw_handle(handle) };
    Ok((file, io_status.information))
}

pub(super) fn writable_relative(parent: &File, name: &[u16], directory: bool) -> io::Result<File> {
    let (file, _) = acquire_with_access(parent, name, directory, None, 0x0012_019f, FILE_OPEN, 7)?;
    check_opened(&file, name, directory)?;
    Ok(file)
}

pub(super) fn private_file(parent: &File, name: &[u16], create: bool) -> io::Result<File> {
    let private = security::PrivateDescriptor::new(false)?;
    let (file, _) = acquire_with_access(
        parent,
        name,
        false,
        create.then_some(&private),
        0x0012_019f,
        if create { FILE_CREATE } else { FILE_OPEN },
        3,
    )?;
    check_opened(&file, name, false)?;
    private.verify(&file)?;
    Ok(file)
}

pub(super) fn rename_child(
    parent: &File,
    source: &[u16],
    destination: &[u16],
    directory: bool,
    replace: bool,
) -> io::Result<()> {
    security::PrivateDescriptor::new(true)?.verify(parent)?;
    match relative(parent, destination, directory) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let (file, _) = acquire_with_access(
        parent,
        source,
        directory,
        None,
        if directory {
            DIRECTORY_READ | 0x30000
        } else {
            FILE_READ | 0x10000
        },
        FILE_OPEN,
        3,
    )?;
    check_opened(&file, source, directory)?;
    security::PrivateDescriptor::new(directory)?.verify(&file)?;
    set_child_name(
        &file,
        parent,
        destination,
        replace,
        if replace && !directory { 65 } else { 10 },
    )
}

fn set_child_name(
    file: &File,
    parent: &File,
    destination: &[u16],
    replace: bool,
    class: u32,
) -> io::Result<()> {
    let pointer = std::mem::size_of::<Handle>();
    let root_offset = pointer;
    let length_offset = root_offset + pointer;
    let name_offset = length_offset + 4;
    let size = name_offset + destination.len() * 2;
    let name_bytes = u32::try_from(destination.len() * 2).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "rename name exceeds native limit",
        )
    })?;
    let information_bytes = u32::try_from(size).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "rename buffer exceeds native limit",
        )
    })?;
    let mut storage = vec![0usize; size.div_ceil(pointer)];
    // SAFETY: The owned zeroed word allocation is pointer-aligned and spans at
    // least size bytes. The FileRenameInformation header uses native pointer
    // alignment; the exact UTF-16 suffix starts immediately after FileNameLength.
    let bytes = unsafe { std::slice::from_raw_parts_mut(storage.as_mut_ptr().cast::<u8>(), size) };
    if class == 65 {
        // FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS. Existing
        // delete-sharing readers keep their object; private writer pins still block.
        bytes[..4].copy_from_slice(&3_u32.to_ne_bytes());
    } else {
        bytes[0] = u8::from(replace);
    }
    bytes[root_offset..length_offset]
        .copy_from_slice(&(parent.as_raw_handle() as usize).to_ne_bytes());
    bytes[length_offset..name_offset].copy_from_slice(&name_bytes.to_ne_bytes());
    for (chunk, unit) in bytes[name_offset..].chunks_exact_mut(2).zip(destination) {
        chunk.copy_from_slice(&unit.to_ne_bytes());
    }
    let mut status = StatusBlock::default();
    // SAFETY: Both owned native handles and the aligned bounded rename buffer
    // remain live through the synchronous call. Rename (10), extended rename (65)
    // and link (11) use the retained RootDirectory and a single relative name,
    // without an absolute path reopen. The extended header uses a four-byte Flags
    // field in the same aligned union as ordinary ReplaceIfExists.
    nt_result(unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &raw mut status,
            bytes.as_ptr().cast(),
            information_bytes,
            class,
        )
    })
}

pub(super) fn link_child(
    parent: &File,
    source_parent: &File,
    source: &[u16],
    destination: &[u16],
) -> io::Result<()> {
    let (file, _) = acquire_with_access(
        source_parent,
        source,
        false,
        None,
        FILE_READ | 0x10000,
        FILE_OPEN,
        3,
    )?;
    check_opened(&file, source, false)?;
    security::PrivateDescriptor::new(false)?.verify(&file)?;
    set_child_name(&file, parent, destination, false, 11)
}

pub(super) fn remove_child(parent: &File, name: &[u16]) -> io::Result<()> {
    remove_entry(parent, name, false)
}

pub(super) fn remove_empty_directory(parent: &File, name: &[u16]) -> io::Result<()> {
    remove_entry(parent, name, true)
}

fn remove_entry(parent: &File, name: &[u16], directory: bool) -> io::Result<()> {
    let (file, _) = acquire_with_access(
        parent,
        name,
        directory,
        None,
        FILE_READ | 0x10000,
        FILE_OPEN,
        3,
    )?;
    check_opened(&file, name, directory)?;
    let mut status = StatusBlock::default();
    let delete = 1_u8;
    // SAFETY: FileDispositionInformation (13) reads one BOOLEAN from live storage.
    // The owned synchronous handle has DELETE access and is only closed afterward.
    nt_result(unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &raw mut status,
            (&raw const delete).cast(),
            1,
            13,
        )
    })
}

pub(super) fn privatize_directory(parent: &File, name: &[u16]) -> io::Result<File> {
    privatize(parent, name, true)
}

pub(super) fn privatize_file(parent: &File, name: &[u16]) -> io::Result<File> {
    privatize(parent, name, false)
}

fn privatize(parent: &File, name: &[u16], directory: bool) -> io::Result<File> {
    let private = security::PrivateDescriptor::new(directory)?;
    let (readonly, _) = acquire_with_access(
        parent,
        name,
        directory,
        None,
        if directory {
            DIRECTORY_READ | 0x20000
        } else {
            FILE_READ
        },
        FILE_OPEN,
        7,
    )?;
    check_opened(&readonly, name, directory)?;
    if private.verify(&readonly).is_ok() {
        return Ok(readonly);
    }
    drop(readonly);
    // DACL replacement requires READ_CONTROL and WRITE_DAC, not file data
    // writes. An executing image can reject the latter while permitting the
    // metadata operation and subsequent handle-based rename.
    let access = if directory {
        0x0016_019f
    } else {
        FILE_READ | 0x40000 // WRITE_DAC
    };
    let (file, _) = acquire_with_access(parent, name, directory, None, access, FILE_OPEN, 7)?;
    check_opened(&file, name, directory)?;
    private.apply(&file)?;
    Ok(file)
}

pub(super) fn pin_namespace(directory: &File) -> io::Result<File> {
    let (file, _) = acquire_with_access(directory, &[], true, None, 0x0012_019f, FILE_OPEN, 3)?;
    check_opened(&file, &[], true)?;
    security::PrivateDescriptor::new(true)?.verify(&file)?;
    if identity(directory)? != identity(&file)? {
        return Err(io::Error::other("namespace identity changed while pinning"));
    }
    Ok(file)
}

pub(super) fn verify_private_directory(directory: &File) -> io::Result<()> {
    let (file, _) = acquire_with_access(
        directory,
        &[],
        true,
        None,
        DIRECTORY_READ | 0x20000,
        FILE_OPEN,
        7,
    )?;
    check_opened(&file, &[], true)?;
    if identity(directory)? != identity(&file)? {
        return Err(io::Error::other("private directory identity changed"));
    }
    security::PrivateDescriptor::new(true)?.verify(&file)
}

pub(super) fn verify_private(file: &File) -> io::Result<()> {
    security::PrivateDescriptor::new(false)?.verify(file)
}

pub(super) fn version(file: &File) -> io::Result<crate::windows::FileVersion> {
    let basic = information(file, 0, 40)?;
    let standard = information(file, 1, 24)?;
    let identity = identity(file)?;
    Ok(crate::windows::FileVersion {
        identity: identity
            .try_into()
            .map_err(|_| io::Error::other("invalid FileID extent"))?,
        attributes: u32::from_ne_bytes(basic[32..36].try_into().expect("attributes")),
        length: u64::from_ne_bytes(standard[8..16].try_into().expect("length")),
        link_count: u32::from_ne_bytes(standard[16..20].try_into().expect("link count")),
        modified: i64::from_ne_bytes(basic[16..24].try_into().expect("last write")),
        changed: i64::from_ne_bytes(basic[24..32].try_into().expect("change time")),
    })
}

fn check_opened(file: &File, name: &[u16], directory: bool) -> io::Result<()> {
    require_type(file, directory)?;
    check_name(file, name)
}

fn check_name(file: &File, name: &[u16]) -> io::Result<()> {
    if !name.is_empty() {
        let actual = long_name(file)?;
        let final_name = actual.rsplit(|unit| *unit == 92).next().unwrap_or_default();
        if final_name != name {
            return Err(names::invalid("exact Windows long-name spelling required"));
        }
    }
    Ok(())
}

pub(super) fn child_kind(parent: &File, name: &[u16]) -> io::Result<crate::EntryKind> {
    // READ_ATTRIBUTES | SYNCHRONIZE observes a final no-follow handle without
    // requesting content access or assuming whether the child is a directory.
    let (file, _) = acquire_with_access(parent, name, None, None, 0x0010_0080, FILE_OPEN, 7)?;
    check_name(&file, name)?;
    let attributes = version(&file)?.attributes;
    Ok(if attributes & 0x400 != 0 {
        crate::EntryKind::Link
    } else if attributes & 0x10 != 0 {
        crate::EntryKind::Directory
    } else {
        crate::EntryKind::File
    })
}

pub(super) fn create_directory(
    parent: &File,
    name: &[u16],
) -> Result<File, crate::DirectoryCreationError> {
    use crate::DirectoryCreationError::{Created, NotCreated};
    let private = security::PrivateDescriptor::new(true).map_err(NotCreated)?;
    let (file, receipt) = acquire(parent, name, true, Some(&private)).map_err(NotCreated)?;
    if receipt != 2 {
        return Err(Created(io::Error::other(
            "native creation receipt was not FILE_CREATED",
        )));
    }
    check_opened(&file, name, true).map_err(Created)?;
    private.verify(&file).map_err(Created)?;
    Ok(file)
}

#[cfg(test)]
pub(super) fn private_descriptor(file: &File) -> io::Result<String> {
    security::observed(file)
}

fn information(file: &File, kind: u32, size: u32) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0_u64; (size as usize).div_ceil(8)];
    // SAFETY: The zeroed allocation is aligned to 8 bytes and holds at least size
    // writable bytes. The handle and allocation outlive the synchronous call.
    let ok = unsafe {
        GetFileInformationByHandleEx(file.as_raw_handle(), kind, buffer.as_mut_ptr().cast(), size)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer
        .into_iter()
        .flat_map(u64::to_ne_bytes)
        .take(size as usize)
        .collect())
}

pub(super) fn identity(file: &File) -> io::Result<Vec<u8>> {
    information(file, 18, 24)
}

fn require_type(file: &File, directory: bool) -> io::Result<()> {
    let bytes = information(file, FILE_ATTRIBUTE_TAG_INFO, 8)?;
    let attributes = u32::from_le_bytes(bytes[..4].try_into().expect("fixed attribute record"));
    if attributes & REPARSE != 0 {
        return Err(io::Error::from_raw_os_error(
            ERROR_REPARSE_POINT_ENCOUNTERED,
        ));
    }
    if (attributes & DIRECTORY != 0) != directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "opened object has the wrong file type",
        ));
    }
    Ok(())
}

fn long_name(file: &File) -> io::Result<Vec<u16>> {
    let bytes = information(file, 2, 65540)?;
    let count = u32::from_le_bytes(bytes[..4].try_into().expect("fixed name header")) as usize;
    if count == 0 || count > bytes.len() - 4 || !count.is_multiple_of(2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid native long-name length",
        ));
    }
    Ok(utf16(&bytes[4..4 + count]))
}

fn utf16(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

#[derive(Debug)]
pub(super) struct EnumerationBuffer {
    words: Vec<u64>,
    used: usize,
    offset: usize,
}

impl EnumerationBuffer {
    pub(super) fn new() -> Self {
        Self {
            words: vec![0; BUFFER_SIZE as usize / 8],
            used: 0,
            offset: 0,
        }
    }

    fn refill(&mut self, file: &File) -> io::Result<bool> {
        let mut io_status = StatusBlock::default();
        #[cfg(test)]
        ENUMERATION_QUERIES.with(|count| count.set(count.get() + 1));
        // SAFETY: file has synchronous directory-list access. The aligned,
        // initialized buffer and status block stay writable until return; no
        // event/APC can outlive them. Class 60 fills a bounded multi-entry batch.
        let status = unsafe {
            NtQueryDirectoryFileEx(
                file.as_raw_handle(),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                &raw mut io_status,
                self.words.as_mut_ptr().cast(),
                BUFFER_SIZE,
                60,
                0,
                std::ptr::null(),
            )
        };
        if status.cast_unsigned() == 0x8000_0006 {
            return Ok(false);
        }
        nt_result(status)?;
        if !(88..=BUFFER_SIZE as usize).contains(&io_status.information) {
            return Err(invalid_record());
        }
        self.used = io_status.information;
        self.offset = 0;
        Ok(true)
    }
}

#[cfg(test)]
thread_local! {
    pub(super) static ENUMERATION_QUERIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn next_entry(
    file: &File,
    buffer: &mut EnumerationBuffer,
) -> io::Result<Option<DirectoryEntry>> {
    loop {
        if buffer.offset == buffer.used && !buffer.refill(file)? {
            return Ok(None);
        }
        // SAFETY: words owns a fully initialized BUFFER_SIZE allocation; refill
        // checked used against that capacity. I/O completed synchronously, and
        // the immutable view ends before any refill. u64/u8 accept every bit pattern.
        let bytes =
            unsafe { std::slice::from_raw_parts(buffer.words.as_ptr().cast::<u8>(), buffer.used) };
        let (entry, offset) = parse_entry(bytes, buffer.offset)?;
        buffer.offset = offset;
        if let Some(entry) = entry {
            return Ok(Some(entry));
        }
    }
}

fn parse_entry(bytes: &[u8], offset: usize) -> io::Result<(Option<DirectoryEntry>, usize)> {
    let record = bytes.get(offset..).ok_or_else(invalid_record)?;
    if record.len() < 88 {
        return Err(invalid_record());
    }
    let next = word(record, 0) as usize;
    let end = if next == 0 {
        record.len()
    } else {
        if !next.is_multiple_of(8) || next < 88 || next > record.len() - 88 {
            return Err(invalid_record());
        }
        next
    };
    let length = word(record, 60) as usize;
    if length == 0 || !length.is_multiple_of(2) || length > end - 88 {
        return Err(invalid_record());
    }
    let units = utf16(&record[88..88 + length]);
    let following = offset + end;
    if units == [46] || units == [46, 46] {
        return Ok((None, following));
    }
    // Enumeration observes names outside the open grammar. Preserve their
    // lossless spelling while rejecting separators/NUL at this native boundary.
    if units.iter().any(|unit| matches!(*unit, 0 | 47 | 92)) {
        return Err(invalid_record());
    }
    let attributes = word(record, 56);
    let kind = if attributes & REPARSE != 0 {
        EntryKind::Link
    } else if attributes & DIRECTORY != 0 {
        EntryKind::Directory
    } else if attributes & FILE_ATTRIBUTE_DEVICE != 0 {
        EntryKind::Other
    } else {
        EntryKind::File
    };
    Ok((
        Some(DirectoryEntry {
            name: OsString::from_wide(&units),
            kind,
        }),
        following,
    ))
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated directory header"),
    )
}

fn invalid_record() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid native directory entry record",
    )
}

#[cfg(test)]
pub(super) fn set_junction(file: &File, target: &[u16]) -> io::Result<()> {
    let prefix = [92, 63, 63, 92]; // native \??\ substitute name
    let substitute: Vec<_> = prefix.into_iter().chain(target.iter().copied()).collect();
    let substitute_bytes = u16::try_from(substitute.len() * 2).expect("fixture substitute length");
    let print_bytes = u16::try_from(target.len() * 2).expect("fixture print length");
    let data_size = 8 + usize::from(substitute_bytes) + 2 + usize::from(print_bytes) + 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0xa000_0003_u32.to_le_bytes());
    bytes.extend_from_slice(
        &u16::try_from(data_size)
            .expect("fixture reparse size")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    for field in [0, substitute_bytes, substitute_bytes + 2, print_bytes] {
        bytes.extend_from_slice(&field.to_le_bytes());
    }
    for unit in substitute
        .into_iter()
        .chain([0])
        .chain(target.iter().copied())
        .chain([0])
    {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let mut returned = 0;
    // SAFETY: The fixture owns a writable handle to its newly created empty directory.
    // The bounded, counted REPARSE_DATA_BUFFER is live for the synchronous call;
    // no output or overlapped work is requested. This only modifies the test fixture.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            0x900a4,
            bytes.as_ptr().cast(),
            u32::try_from(bytes.len()).expect("fixture buffer size"),
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod record_tests {
    use super::*;

    fn record(name: &[u16], next: u32) -> Vec<u8> {
        let mut bytes = vec![0; 88 + name.len() * 2];
        bytes[..4].copy_from_slice(&next.to_le_bytes());
        bytes[60..64].copy_from_slice(&u32::try_from(name.len() * 2).unwrap().to_le_bytes());
        for (index, unit) in name.iter().enumerate() {
            bytes[88 + index * 2..90 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn records_follow_aligned_offsets_and_preserve_native_names() {
        let mut bytes = record(&[46], 96);
        bytes.resize(96, 0);
        bytes.extend(record(&[0xd800, 0x4e2d], 0));
        let (dot, offset) = parse_entry(&bytes, 0).unwrap();
        assert!(dot.is_none());
        assert_eq!(offset, 96);
        let (entry, offset) = parse_entry(&bytes, offset).unwrap();
        assert_eq!(entry.unwrap().name, OsString::from_wide(&[0xd800, 0x4e2d]));
        assert_eq!(offset, bytes.len());
    }

    #[test]
    fn malformed_records_cannot_cross_headers_names_or_batch_bounds() {
        let valid = record(&[65], 0);
        for length in 0..valid.len() {
            assert_eq!(
                parse_entry(&valid[..length], 0).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        for next in [8_u32, 88, 90, 96, u32::MAX] {
            assert!(parse_entry(&record(&[65], next), 0).is_err());
        }
        for length in [0_u32, 1, 4, u32::MAX] {
            let mut bytes = valid.clone();
            bytes[60..64].copy_from_slice(&length.to_le_bytes());
            assert!(parse_entry(&bytes, 0).is_err());
        }
        for name in [0_u16, 47, 92] {
            assert!(parse_entry(&record(&[name], 0), 0).is_err());
        }
        assert!(parse_entry(&valid, usize::MAX).is_err());
        let mut bytes = record(&[65; 8], 96);
        bytes.resize(184, 0);
        assert!(
            parse_entry(&bytes, 0).is_err(),
            "a name cannot overlap the next record"
        );
    }
}
