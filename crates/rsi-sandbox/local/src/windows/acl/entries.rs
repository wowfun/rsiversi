use super::{
    ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE, ACCESS_MODE, ACL, EXPLICIT_ACCESS_W, EqualSid,
    GRANT_MASK, PSID, SYSTEM_MANDATORY_LABEL_ACE_TYPE, Security, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W, check, io,
};
use windows_sys::Win32::Security::{ACCESS_ALLOWED_ACE, ACE_HEADER, GetAce, IsValidSid};

pub(super) fn entry(
    sid: PSID,
    mode: ACCESS_MODE,
    mask: u32,
    inheritance: u32,
) -> EXPLICIT_ACCESS_W {
    EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: mode,
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.cast(),
            ..Default::default()
        },
    }
}
struct Ace<'a> {
    kind: u32,
    flags: u32,
    mask: u32,
    sid: PSID,
    _descriptor: std::marker::PhantomData<&'a Security>,
}
fn any<'a>(
    owner: &'a Security,
    acl: *mut ACL,
    mut matches: impl FnMut(Ace<'a>) -> bool,
) -> io::Result<bool> {
    let _ = owner;
    if acl.is_null() {
        return Ok(false);
    }
    // SAFETY: The descriptor retains this ACL returned by GetNamedSecurityInfoW.
    let count = unsafe { (*acl).AceCount };
    for index in 0..u32::from(count) {
        let mut pointer = std::ptr::null_mut();
        // SAFETY: The retained ACL and in-range entry index identify an owned ACE.
        check(unsafe { GetAce(acl, index, &raw mut pointer) }, "GetAce")?;
        // SAFETY: GetAce returned at least the initialized ACE header.
        let header = unsafe { &*pointer.cast::<ACE_HEADER>() };
        if ![
            ACCESS_ALLOWED_ACE_TYPE,
            ACCESS_DENIED_ACE_TYPE,
            SYSTEM_MANDATORY_LABEL_ACE_TYPE,
        ]
        .contains(&u32::from(header.AceType))
        {
            continue;
        }
        let offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
        if usize::from(header.AceSize) < offset + 8 {
            return Err(io::Error::other("truncated ACL SID"));
        }
        // SAFETY: The fixed fields and SID header lie within the checked ACE.
        let (mask, sid, count) = unsafe {
            let record = pointer.cast::<ACCESS_ALLOWED_ACE>();
            let sid = pointer.cast::<u8>().add(offset);
            ((*record).Mask, sid.cast(), *sid.add(1))
        };
        if offset + 8 + usize::from(count) * 4 > usize::from(header.AceSize) {
            return Err(io::Error::other("truncated ACL subauthorities"));
        }
        // SAFETY: The complete counted SID lies inside the retained ACE.
        check(unsafe { IsValidSid(sid) }, "IsValidSid(ACL)")?;
        if matches(Ace {
            kind: u32::from(header.AceType),
            flags: u32::from(header.AceFlags),
            mask,
            sid,
            _descriptor: std::marker::PhantomData,
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(super) fn exact(
    owner: &Security,
    acl: *mut ACL,
    kind: u32,
    flags: u32,
    mask: u32,
    sid: PSID,
) -> io::Result<bool> {
    any(owner, acl, |ace| {
        // SAFETY: Both complete SIDs remain owned by the descriptor or caller.
        ace.kind == kind
            && ace.flags == flags
            && ace.mask == mask
            && unsafe { EqualSid(ace.sid, sid) } != 0
    })
}
pub(super) fn foreign_grant(owner: &Security, sid: PSID) -> io::Result<bool> {
    any(owner, owner.dacl, |ace| {
        // SAFETY: Both complete SIDs remain owned by the descriptor or caller.
        ace.kind == ACCESS_ALLOWED_ACE_TYPE
            && ace.mask == GRANT_MASK
            && unsafe { EqualSid(ace.sid, sid) } == 0
    })
}
