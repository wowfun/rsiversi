//! Idempotent ACL edits. Shared workspace grants remain standing and serialized.
use super::{
    native::{Local, check, information, sid, status, wide},
    token,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use windows_sys::Win32::{
    Security::{
        ACL, ACL_REVISION, AddMandatoryAce,
        Authorization::{
            ACCESS_MODE, DENY_ACCESS, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW,
            REVOKE_ACCESS, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
            TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
        },
        CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid, GetLengthSid, InitializeAcl,
        LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSID,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT, SYSTEM_MANDATORY_LABEL_ACE, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        DELETE, FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_WRITE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, STANDARD_RIGHTS_WRITE, WRITE_OWNER,
    },
    System::SystemServices::{
        ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE, SYSTEM_MANDATORY_LABEL_ACE_TYPE,
        SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
    },
};
mod entries;
#[cfg(test)]
mod tests;
use entries::{entry, exact, foreign_grant};
const GRANT_MASK: u32 = (FILE_GENERIC_WRITE | DELETE | FILE_DELETE_CHILD) & !STANDARD_RIGHTS_WRITE;
const INHERIT: u32 = SUB_CONTAINERS_AND_OBJECTS_INHERIT;

struct Security {
    _owner: Local,
    owner: PSID,
    dacl: *mut ACL,
    label: *mut ACL,
}
impl Security {
    fn read(path: &Path) -> io::Result<Self> {
        let text = wide(path.as_os_str())?;
        let mut owner = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let mut label = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: The terminated path and result slots are live. The returned
        // descriptor owns all referenced security records until LocalFree.
        status(
            unsafe {
                GetNamedSecurityInfoW(
                    text.as_ptr(),
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION
                        | DACL_SECURITY_INFORMATION
                        | LABEL_SECURITY_INFORMATION,
                    &raw mut owner,
                    std::ptr::null_mut(),
                    &raw mut dacl,
                    &raw mut label,
                    &raw mut descriptor,
                )
            },
            "GetNamedSecurityInfoW",
        )?;
        Ok(Self {
            _owner: Local(descriptor),
            owner,
            dacl,
            label,
        })
    }
}
fn lock(path: &Path) -> io::Result<File> {
    let root = std::env::temp_dir().join("dsh-acl-locks");
    std::fs::create_dir_all(&root)?;
    let path = path
        .to_str()
        .ok_or_else(|| io::Error::other("Windows ACL path is not Unicode"))?;
    let digest = hex::encode(Sha256::digest(path.to_lowercase().as_bytes()));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(root.join(format!("{}.lock", &digest[..16])))?;
    // File::lock uses LockFileEx; its range overlaps DSH's byte-zero lock.
    // Omitting FILE_SHARE_DELETE prevents pathname replacement while locked.
    file.lock()?;
    Ok(file)
}
#[derive(Clone, Copy)]
enum Label {
    Set(*mut ACL),
    Clear,
    Keep,
}
fn apply(
    path: &Path,
    current: &Security,
    entries: &[EXPLICIT_ACCESS_W],
    label: Label,
) -> io::Result<()> {
    let mut merged = std::ptr::null_mut();
    // SAFETY: Each entry SID and the original ACL are retained through the merge.
    status(
        unsafe {
            SetEntriesInAclW(
                u32::try_from(entries.len()).expect("fixed ACL edit set"),
                entries.as_ptr(),
                current.dacl,
                &raw mut merged,
            )
        },
        "SetEntriesInAclW(directory)",
    )?;
    let merged = Local(merged.cast());
    let text = wide(path.as_os_str())?;
    let (flags, label) = match label {
        Label::Set(label) => (
            DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            label,
        ),
        Label::Clear => (
            DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
        ),
        Label::Keep => (DACL_SECURITY_INFORMATION, std::ptr::null_mut()),
    };
    // SAFETY: Both complete ACLs and path remain live. Owner/group are unchanged.
    status(
        unsafe {
            SetNamedSecurityInfoW(
                text.as_ptr(),
                SE_FILE_OBJECT,
                flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                merged.0.cast(),
                label,
            )
        },
        "SetNamedSecurityInfoW",
    )
}
fn label_right(path: &Path, current: &Security) -> io::Result<bool> {
    match OpenOptions::new()
        .access_mode(WRITE_OWNER)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
    {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
        Err(error) => return Err(error),
    }
    let token = token::current()?;
    let data = information(&token, TokenUser)?;
    // SAFETY: TokenUser initialized this aligned record and its referenced SID.
    let user = unsafe { &*data.as_ptr().cast::<TOKEN_USER>() };
    // SAFETY: Both owner and token SIDs remain within their owned allocations.
    if current.owner.is_null() || unsafe { EqualSid(current.owner, user.User.Sid) } == 0 {
        return Err(io::Error::other(
            "setting the Low label requires WRITE_OWNER; directory has another owner",
        ));
    }
    apply(
        path,
        current,
        &[entry(
            user.User.Sid,
            GRANT_ACCESS,
            WRITE_OWNER,
            SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        )],
        Label::Keep,
    )?;
    Ok(true)
}
pub(super) fn write_sid(path: &Path, temporary: bool) -> io::Result<String> {
    let mut hash = Sha256::new();
    if temporary {
        hash.update(b"temp\0");
    }
    hash.update(
        path.to_str()
            .ok_or_else(|| io::Error::other("Windows ACL path is not Unicode"))?
            .as_bytes(),
    );
    let bytes = hash.finalize();
    let part = |offset| {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("SHA-256 slice"))
            % ((1 << 30) - 1)
            + 1
    };
    Ok(format!(
        "S-1-4-{}-{}{}",
        part(0),
        part(4),
        if temporary { "-1" } else { "" }
    ))
}
pub(super) fn grant_workspace(path: &Path, capability: &Local) -> io::Result<bool> {
    let _lock = lock(path)?;
    grant_private(path, capability)
}
// The caller either owns a fresh directory or holds the workspace lock.
pub(super) fn grant_private(path: &Path, capability: &Local) -> io::Result<bool> {
    let current = Security::read(path)?;
    if current.dacl.is_null() {
        return Err(io::Error::other(
            "directory has no DACL; cannot preserve unrestricted access while adding a capability",
        ));
    }
    let world = sid("S-1-1-0")?;
    let low = sid("S-1-16-4096")?;
    if exact(
        &current,
        current.dacl,
        ACCESS_ALLOWED_ACE_TYPE,
        INHERIT,
        GRANT_MASK,
        capability.0,
    )? && exact(
        &current,
        current.dacl,
        ACCESS_DENIED_ACE_TYPE,
        CONTAINER_INHERIT_ACE,
        FILE_DELETE_CHILD,
        world.0,
    )? && exact(
        &current,
        current.label,
        SYSTEM_MANDATORY_LABEL_ACE_TYPE,
        INHERIT,
        SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
        low.0,
    )? {
        return Ok(false);
    }
    // A missing label right may have merged an owner ACE. Read that current DACL
    // rather than overwriting the repair. Ordinary grants need only one read.
    let current = if label_right(path, &current)? {
        Security::read(path)?
    } else {
        current
    };
    // SAFETY: The Low SID is a complete owned SID.
    let bytes = size_of::<ACL>() + size_of::<SYSTEM_MANDATORY_LABEL_ACE>() - size_of::<u32>()
        + unsafe { GetLengthSid(low.0) } as usize;
    let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    let label = storage.as_mut_ptr().cast::<ACL>();
    // SAFETY: The aligned writable buffer fits the ACL and its complete ACE.
    check(
        unsafe {
            InitializeAcl(
                label,
                u32::try_from(bytes).expect("bounded label ACL"),
                ACL_REVISION,
            )
        },
        "InitializeAcl",
    )?;
    // SAFETY: Both the initialized ACL and complete Low SID remain live.
    check(
        unsafe {
            AddMandatoryAce(
                label,
                ACL_REVISION,
                SUB_CONTAINERS_AND_OBJECTS_INHERIT,
                SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
                low.0,
            )
        },
        "AddMandatoryAce",
    )?;
    apply(
        path,
        &current,
        &[
            entry(
                world.0,
                DENY_ACCESS,
                FILE_DELETE_CHILD,
                CONTAINER_INHERIT_ACE,
            ),
            entry(
                capability.0,
                GRANT_ACCESS,
                GRANT_MASK,
                SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            ),
        ],
        Label::Set(label),
    )?;
    Ok(true)
}
// Called only at the last release of an exclusively owned private directory.
pub(super) fn revoke_private(path: &Path, capability: &Local) -> io::Result<()> {
    let current = Security::read(path)?;
    if current.dacl.is_null() {
        return Ok(());
    }
    let label = if foreign_grant(&current, capability.0)? {
        Label::Keep
    } else {
        Label::Clear
    };
    apply(
        path,
        &current,
        &[entry(capability.0, REVOKE_ACCESS, 0, 0)],
        label,
    )
}
pub(super) fn canonical(path: PathBuf) -> PathBuf {
    rsi_workspace_path::canonical_native_path(path)
}
