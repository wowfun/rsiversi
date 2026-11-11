//! DSH's dual write-restriction list, Low integrity and child-object default DACL.
use super::native::{Local, check, information, sid, status};
use std::{
    io,
    mem::offset_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use windows_sys::Win32::{
    Security::{
        Authorization::{
            EXPLICIT_ACCESS_W, GRANT_ACCESS, SetEntriesInAclW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
            TRUSTEE_W,
        },
        CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, GetLengthSid, SID_AND_ATTRIBUTES,
        SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL,
        TOKEN_DUPLICATE, TOKEN_GROUPS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenDefaultDacl,
        TokenGroups, TokenIntegrityLevel, WRITE_RESTRICTED,
    },
    Storage::FileSystem::FILE_ALL_ACCESS,
    System::{
        SystemServices::{SE_GROUP_INTEGRITY, SE_GROUP_LOGON_ID},
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

pub(super) fn current() -> io::Result<OwnedHandle> {
    let mut handle = std::ptr::null_mut();
    // SAFETY: The current-process pseudo-handle and writable result slot are live.
    check(
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY,
                &raw mut handle,
            )
        },
        "OpenProcessToken",
    )?;
    // SAFETY: Success transfers one new token handle to this owner.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

pub(super) fn restricted(primary: &Local, additional: Option<&Local>) -> io::Result<OwnedHandle> {
    let parent = current()?;
    let groups = information(&parent, TokenGroups)?;
    // SAFETY: TokenGroups initializes its count in this aligned OS-sized buffer.
    let count = unsafe { *groups.as_ptr().cast::<u32>() } as usize;
    let start = offset_of!(TOKEN_GROUPS, Groups);
    if start + count * size_of::<SID_AND_ATTRIBUTES>() > groups.len() * size_of::<usize>() {
        return Err(io::Error::other("invalid TokenGroups array size"));
    }
    // SAFETY: The checked, aligned array and all pointed-to SIDs remain in groups.
    let entries = unsafe {
        std::slice::from_raw_parts(
            groups.as_ptr().byte_add(start).cast::<SID_AND_ATTRIBUTES>(),
            count,
        )
    };
    let logon = entries
        .iter()
        .find(|entry| {
            entry.Attributes & SE_GROUP_LOGON_ID.cast_unsigned()
                == SE_GROUP_LOGON_ID.cast_unsigned()
        })
        .ok_or_else(|| io::Error::other("current token has no logon SID"))?;
    let world = sid("S-1-1-0")?;
    let sids = [
        logon.Sid,
        world.0,
        primary.0,
        additional.unwrap_or(primary).0,
    ]
    .map(|sid| SID_AND_ATTRIBUTES {
        Sid: sid,
        Attributes: 0,
    });
    let mut handle = std::ptr::null_mut();
    // SAFETY: The parent, aligned SID array and each SID stay live through this
    // copying call. The nonempty list always includes the required ambient SIDs.
    check(
        unsafe {
            CreateRestrictedToken(
                parent.as_raw_handle(),
                DISABLE_MAX_PRIVILEGE | WRITE_RESTRICTED,
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                if additional.is_some() { 4 } else { 3 },
                sids.as_ptr(),
                &raw mut handle,
            )
        },
        "CreateRestrictedToken",
    )?;
    // SAFETY: Successful token creation transfers this handle exactly once.
    let token = unsafe { OwnedHandle::from_raw_handle(handle) };
    lower_integrity(&token)?;
    default_dacl(&token, primary)?;
    Ok(token)
}

fn lower_integrity(token: &OwnedHandle) -> io::Result<()> {
    let low = sid("S-1-16-4096")?;
    // SAFETY: ConvertStringSidToSidW supplied a complete live SID.
    let sid_bytes = unsafe { GetLengthSid(low.0) } as usize;
    let bytes = size_of::<TOKEN_MANDATORY_LABEL>() + sid_bytes;
    let mut record = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    // SAFETY: The aligned allocation is large enough for the record and the
    // API-required SID-size suffix; the separately owned SID remains live.
    unsafe {
        record
            .as_mut_ptr()
            .cast::<TOKEN_MANDATORY_LABEL>()
            .write(TOKEN_MANDATORY_LABEL {
                Label: SID_AND_ATTRIBUTES {
                    Sid: low.0,
                    Attributes: SE_GROUP_INTEGRITY.cast_unsigned(),
                },
            });
    }
    // SAFETY: Token, complete record and SID remain live for this copying call.
    check(
        unsafe {
            SetTokenInformation(
                token.as_raw_handle(),
                TokenIntegrityLevel,
                record.as_ptr().cast(),
                u32::try_from(bytes).expect("fixed label record and bounded SID"),
            )
        },
        "SetTokenInformation(Low)",
    )
}

fn default_dacl(token: &OwnedHandle, capability: &Local) -> io::Result<()> {
    let data = information(token, TokenDefaultDacl)?;
    // SAFETY: TokenDefaultDacl initialized this aligned record and its ACL.
    let old = unsafe { &*data.as_ptr().cast::<TOKEN_DEFAULT_DACL>() };
    if old.DefaultDacl.is_null() {
        return Err(io::Error::other("current token has no default DACL"));
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: 0,
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: capability.0.cast(),
            ..Default::default()
        },
    };
    let mut merged = std::ptr::null_mut();
    // SAFETY: Old ACL and capability SID stay live; success allocates one ACL.
    status(
        unsafe { SetEntriesInAclW(1, &raw const entry, old.DefaultDacl, &raw mut merged) },
        "SetEntriesInAclW(token)",
    )?;
    let merged = Local(merged.cast());
    let record = TOKEN_DEFAULT_DACL {
        DefaultDacl: merged.0.cast(),
    };
    // SAFETY: The record and complete ACL stay live until the token copies them.
    check(
        unsafe {
            SetTokenInformation(
                token.as_raw_handle(),
                TokenDefaultDacl,
                (&raw const record).cast(),
                u32::try_from(size_of::<TOKEN_DEFAULT_DACL>()).expect("fixed Win32 record"),
            )
        },
        "SetTokenInformation(default DACL)",
    )
}
