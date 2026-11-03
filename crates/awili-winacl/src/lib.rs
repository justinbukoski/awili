//! Owner-only access control for files and folders on Windows: the equivalent
//! of Unix mode 0600 (files) and 0700 (folders).
//!
//! The shape is one protected DACL (inheritance from the parent switched off)
//! that grants full control to exactly two principals: the current user (from
//! the process token) and LOCAL SYSTEM. Folders carry it as object-inherit and
//! container-inherit, so what is created inside inherits it.
//!
//! - [`create_new_file_owner_only`] and [`create_dir_owner_only`] create the
//!   object WITH that descriptor (`CreateFileW` / `CreateDirectoryW` with
//!   `SECURITY_ATTRIBUTES`), so there is no instant at which it exists with a
//!   broader list, then read it back.
//! - [`restrict_to_owner`] applies it to something that already exists.
//! - [`verify_owner_only`] reads the descriptor back and requires exactly what
//!   the two calls above build: protected, exactly user + SYSTEM with full
//!   control, `OI|CI` on folders, and the user as the owner (the
//!   Administrators group is accepted as owner only when the process is
//!   elevated, because an elevated token owns what it creates as that group).
//!
//! A volume without file permissions (FAT, exFAT) cannot hold any of this. Every
//! function then fails with [`NTFS_NEEDED`], never silently succeeds.
//!
//! On every other platform this crate is empty; callers gate on
//! `cfg(windows)` and use their existing mode handling instead.

#![cfg(windows)]

use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS,
    ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE,
    HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetTokenInformation,
    TokenElevation, TokenUser, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
    SE_DACL_PROTECTED, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, GetVolumeInformationW, GetVolumePathNameW, CREATE_NEW,
    FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// What the person is told when the profile folder's volume has no file
/// permissions, so keys cannot be kept private there.
pub const NTFS_NEEDED: &str = "Awili needs an NTFS profile folder to keep your keys private";

/// `FILE_PERSISTENT_ACLS`: the volume keeps file permissions.
const FILE_PERSISTENT_ACLS: u32 = 0x0000_0008;
/// `FILE_ALL_ACCESS`.
const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
/// `ACCESS_ALLOWED_ACE_TYPE`.
const ACE_ALLOWED: u8 = 0;
/// `OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`.
const OI_CI: u8 = 0x03;
/// SDDL for LOCAL SYSTEM.
const SYSTEM_SID: &str = "S-1-5-18";
/// BUILTIN\Administrators.
const ADMINS_SID: &str = "S-1-5-32-544";

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn last_error(what: &str) -> io::Error {
    // SAFETY: GetLastError has no preconditions.
    let code = unsafe { GetLastError() };
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{what} failed (Windows error {code})"),
    )
}

fn win32(code: u32, what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{what} failed (Windows error {code})"),
    )
}

fn denied(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why.into())
}

/// Whether the volume holding `path` keeps file permissions (NTFS, ReFS).
/// `path` may be a file, a folder, or a folder that does not exist yet (the
/// nearest existing parent is asked). `None` when the volume cannot be asked.
pub fn volume_supports_acls(path: &Path) -> Option<bool> {
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        probe = probe.parent()?.to_path_buf();
    }
    let probe_w = wide(&probe);
    let mut root = vec![0u16; 1024];
    // SAFETY: probe_w is NUL-terminated and root has the stated length.
    if unsafe { GetVolumePathNameW(probe_w.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return None;
    }
    let mut flags = 0u32;
    // SAFETY: root is NUL-terminated; unused out parameters are null.
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut flags,
            null_mut(),
            0,
        )
    };
    (ok != 0).then_some(flags & FILE_PERSISTENT_ACLS != 0)
}

/// Turn a failure while protecting `path` into the plain message when the real
/// reason is a volume without file permissions; any other error passes through.
fn explain(path: &Path, err: io::Error) -> io::Error {
    if volume_supports_acls(path) == Some(false) {
        return io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{NTFS_NEEDED} ({} is on a volume without file permissions)",
                path.display()
            ),
        );
    }
    err
}

/// A SID buffer owned by this module (copy of the token user's SID).
struct OwnedSid(Vec<u8>);

impl OwnedSid {
    fn as_ptr(&self) -> *mut c_void {
        self.0.as_ptr() as *mut c_void
    }
}

/// Run `f` with the process token opened for query.
fn with_token<R>(f: impl FnOnce(HANDLE) -> io::Result<R>) -> io::Result<R> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo handle from GetCurrentProcess is valid; token is a
    // valid out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error("OpenProcessToken"));
    }
    let result = f(token);
    // SAFETY: token was opened above.
    unsafe { CloseHandle(token) };
    result
}

fn current_user_sid() -> io::Result<OwnedSid> {
    with_token(|token| {
        let mut needed = 0u32;
        // SAFETY: a size probe with a null buffer, as documented.
        unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed) };
        if needed == 0 {
            return Err(last_error("GetTokenInformation(size)"));
        }
        // u64 backing keeps the buffer 8-byte aligned for TOKEN_USER.
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: buf is at least `needed` bytes.
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr() as *mut c_void,
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(last_error("GetTokenInformation"));
        }
        // SAFETY: the call above filled a TOKEN_USER at the start of buf.
        let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        let sid = user.User.Sid;
        // SAFETY: sid points into buf and is a valid SID.
        let len = unsafe { windows_sys::Win32::Security::GetLengthSid(sid) } as usize;
        // SAFETY: len bytes are readable at sid.
        let bytes = unsafe { std::slice::from_raw_parts(sid as *const u8, len) }.to_vec();
        Ok(OwnedSid(bytes))
    })
}

/// Whether this process runs with an elevated (administrator) token.
pub fn process_is_elevated() -> bool {
    with_token(|token| {
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut got = 0u32;
        // SAFETY: e is a TOKEN_ELEVATION of the stated size.
        if unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                &mut e as *mut _ as *mut c_void,
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut got,
            )
        } == 0
        {
            return Err(last_error("GetTokenInformation(elevation)"));
        }
        Ok(e.TokenIsElevated != 0)
    })
    .unwrap_or(false)
}

fn sid_to_string(sid: *mut c_void) -> io::Result<String> {
    let mut out: *mut u16 = null_mut();
    // SAFETY: sid is a valid SID; out receives a LocalAlloc'd string.
    if unsafe { ConvertSidToStringSidW(sid, &mut out) } == 0 {
        return Err(last_error("ConvertSidToStringSidW"));
    }
    let mut len = 0;
    // SAFETY: out is a NUL-terminated wide string.
    unsafe {
        while *out.add(len) != 0 {
            len += 1;
        }
    }
    // SAFETY: len code units are readable.
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(out, len) });
    // SAFETY: out was allocated by LocalAlloc.
    unsafe { LocalFree(out as *mut c_void) };
    Ok(s)
}

/// The current user's SID as a string such as `S-1-5-21-...`.
pub fn current_user_sid_string() -> io::Result<String> {
    sid_to_string(current_user_sid()?.as_ptr())
}

/// The owner-only security descriptor, built once from SDDL and freed on drop.
struct OwnerSd(*mut c_void);

impl OwnerSd {
    /// `D:P(A;<OICI>;FA;;;<user>)(A;<OICI>;FA;;;SY)`: D = DACL, P = protected
    /// (no inheritance), FA = file all access, SY = SYSTEM.
    fn new(is_dir: bool) -> io::Result<OwnerSd> {
        let user = current_user_sid_string()?;
        let inherit = if is_dir { "OICI" } else { "" };
        let sddl = format!("D:P(A;{inherit};FA;;;{user})(A;{inherit};FA;;;{SYSTEM_SID})");
        let sddl_w = wide_str(&sddl);
        let mut sd: *mut c_void = null_mut();
        // SAFETY: sddl_w is NUL-terminated; sd receives a LocalAlloc'd descriptor.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl_w.as_ptr(),
                1,
                &mut sd,
                null_mut(),
            )
        } == 0
        {
            return Err(last_error(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW",
            ));
        }
        Ok(OwnerSd(sd))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for OwnerSd {
    fn drop(&mut self) {
        // SAFETY: allocated by the convert call.
        unsafe { LocalFree(self.0) };
    }
}

/// Test seam: while armed on this thread, the read-back after a create fails,
/// as it would on a volume that ignores descriptors.
#[cfg(any(test, feature = "test-seam"))]
pub mod seam {
    use std::cell::Cell;
    thread_local! { static FAIL: Cell<bool> = const { Cell::new(false) }; }
    /// Arm or disarm the forced failure on the current thread.
    pub fn force_verify_failure(on: bool) {
        FAIL.with(|f| f.set(on));
    }
    pub(crate) fn armed() -> bool {
        FAIL.with(|f| f.get())
    }
}

fn verify_after_create(path: &Path) -> io::Result<()> {
    #[cfg(any(test, feature = "test-seam"))]
    if seam::armed() {
        return Err(denied("forced failure (test seam)"));
    }
    verify_owner_only(path)
}

/// Create a NEW file (failing if it exists) that is owner-only from its first
/// instant: the descriptor is passed to `CreateFileW`. Read back before
/// returning; on any mismatch (a volume that ignores descriptors) the file is
/// removed and the error says why. The handle is read/write, share-read.
pub fn create_new_file_owner_only(path: &Path) -> io::Result<std::fs::File> {
    let sd = OwnerSd::new(false)?;
    let sa = sd.attributes();
    let path_w = wide(path);
    // SAFETY: path_w is NUL-terminated and sa lives for the call.
    let h = unsafe {
        CreateFileW(
            path_w.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ,
            &sa,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: no preconditions.
        let code = unsafe { GetLastError() };
        if code == ERROR_FILE_EXISTS || code == ERROR_ALREADY_EXISTS {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    // SAFETY: h is a valid handle we own; File takes it over.
    let file = unsafe { std::fs::File::from_raw_handle(h as _) };
    if let Err(e) = verify_after_create(path) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(explain(path, e));
    }
    Ok(file)
}

/// Create ONE new folder (its parent must exist) that is owner-only from its
/// first instant, inheritable by what is created inside. Read back; removed
/// again on a mismatch.
pub fn create_dir_owner_only(path: &Path) -> io::Result<()> {
    let sd = OwnerSd::new(true)?;
    let sa = sd.attributes();
    let path_w = wide(path);
    // SAFETY: path_w is NUL-terminated and sa lives for the call.
    if unsafe { CreateDirectoryW(path_w.as_ptr(), &sa) } == 0 {
        // SAFETY: no preconditions.
        let code = unsafe { GetLastError() };
        if code == ERROR_ALREADY_EXISTS {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    if let Err(e) = verify_after_create(path) {
        let _ = std::fs::remove_dir(path);
        return Err(explain(path, e));
    }
    Ok(())
}

/// Replace the DACL of an EXISTING `path` with the protected, owner-only one.
/// For a folder (`is_dir`) the grants are inherited by everything inside it.
pub fn restrict_to_owner(path: &Path, is_dir: bool) -> io::Result<()> {
    let sd = OwnerSd::new(is_dir)?;
    let mut present = 0i32;
    let mut defaulted = 0i32;
    let mut dacl: *mut ACL = null_mut();
    // SAFETY: sd is a valid self-relative descriptor.
    if unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted) } == 0
        || present == 0
        || dacl.is_null()
    {
        return Err(last_error("GetSecurityDescriptorDacl"));
    }
    let path_w = wide(path);
    // SAFETY: path_w is NUL-terminated and dacl is a valid ACL.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            path_w.as_ptr() as *mut u16,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null_mut(),
        )
    };
    if rc != ERROR_SUCCESS {
        let e = win32(rc, "SetNamedSecurityInfoW");
        if rc == ERROR_INVALID_FUNCTION || rc == ERROR_NOT_SUPPORTED {
            return Err(explain(path, e));
        }
        return Err(explain(path, e));
    }
    Ok(())
}

/// One access-allowed entry read back from a DACL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub sid: String,
    pub mask: u32,
    /// The ACE flags (`OI`/`CI`/inherited bits).
    pub flags: u8,
}

/// What [`read_security`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Security {
    pub protected: bool,
    pub grants: Vec<Grant>,
    pub owner: String,
}

/// The owner and DACL of `path`. Errors on any entry that is not
/// access-allowed (deny, audit...) or on a NULL DACL (open to everyone).
pub fn read_security(path: &Path) -> io::Result<Security> {
    let path_w = wide(path);
    let mut dacl: *mut ACL = null_mut();
    let mut owner: *mut c_void = null_mut();
    let mut sd: *mut c_void = null_mut();
    // SAFETY: path_w is NUL-terminated; out pointers are valid.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            path_w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(win32(rc, "GetNamedSecurityInfoW"));
    }
    let result = (|| {
        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: sd is a valid descriptor returned above.
        if unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) } == 0 {
            return Err(last_error("GetSecurityDescriptorControl"));
        }
        let protected = control & SE_DACL_PROTECTED != 0;
        if dacl.is_null() {
            return Err(denied("object has a NULL DACL (open to everyone)"));
        }
        if owner.is_null() {
            return Err(denied("object has no owner"));
        }
        let owner = sid_to_string(owner)?;
        // SAFETY: dacl is a valid ACL inside sd.
        let count = unsafe { (*dacl).AceCount } as u32;
        let mut grants = Vec::new();
        for i in 0..count {
            let mut ace: *mut c_void = null_mut();
            // SAFETY: i < AceCount.
            if unsafe { GetAce(dacl, i, &mut ace) } == 0 {
                return Err(last_error("GetAce"));
            }
            // SAFETY: GetAce returned a valid ACE header.
            let header = unsafe { &*(ace as *const ACE_HEADER) };
            if header.AceType != ACE_ALLOWED {
                return Err(denied(format!("unexpected ACE type {}", header.AceType)));
            }
            // SAFETY: an ACCESS_ALLOWED_ACE: mask then the SID in SidStart.
            let a = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
            let sid_ptr = &a.SidStart as *const u32 as *mut c_void;
            grants.push(Grant {
                sid: sid_to_string(sid_ptr)?,
                mask: a.Mask,
                flags: header.AceFlags,
            });
        }
        Ok(Security {
            protected,
            grants,
            owner,
        })
    })();
    // SAFETY: sd was allocated by GetNamedSecurityInfoW.
    unsafe { LocalFree(sd) };
    result
}

/// The DACL half of [`read_security`]: protected flag and entries.
pub fn read_dacl(path: &Path) -> io::Result<(bool, Vec<Grant>)> {
    read_security(path).map(|s| (s.protected, s.grants))
}

/// Judge a read-back against what this crate builds. Pure over its inputs so
/// every branch is tested. `elevated` decides whether the Administrators group
/// counts as an acceptable owner.
pub fn judge(sec: &Security, user: &str, is_dir: bool, elevated: bool) -> Result<(), String> {
    if !sec.protected {
        return Err("the access list still inherits from its folder".into());
    }
    if sec.grants.len() != 2 {
        return Err(format!(
            "the access list has {} entries, expected exactly you and SYSTEM",
            sec.grants.len()
        ));
    }
    let want_flags = if is_dir { OI_CI } else { 0 };
    let (mut user_seen, mut system_seen) = (false, false);
    for g in &sec.grants {
        if g.sid == user {
            user_seen = true;
        } else if g.sid == SYSTEM_SID {
            system_seen = true;
        } else {
            return Err(format!("access is also granted to {}", g.sid));
        }
        if g.mask & FILE_ALL_ACCESS != FILE_ALL_ACCESS {
            return Err(format!("{} lacks full control (mask {:#x})", g.sid, g.mask));
        }
        if g.flags != want_flags {
            return Err(format!(
                "{} has inheritance flags {:#x}, expected {:#x}",
                g.sid, g.flags, want_flags
            ));
        }
    }
    if !(user_seen && system_seen) {
        return Err("the access list is not exactly you and SYSTEM".into());
    }
    if sec.owner == user {
        return Ok(());
    }
    if sec.owner == ADMINS_SID && elevated {
        // An elevated process creates objects owned by the Administrators
        // group; that is the same person at the same privilege.
        return Ok(());
    }
    Err(format!("it is owned by {}, not by you", sec.owner))
}

/// Check that `path` is exactly what [`create_new_file_owner_only`] /
/// [`create_dir_owner_only`] build: see [`judge`]. `Err` carries the plain
/// reason it is not.
pub fn verify_owner_only(path: &Path) -> io::Result<()> {
    let user = current_user_sid_string()?;
    let sec = read_security(path)?;
    let is_dir = std::fs::symlink_metadata(path)
        .map(|m| m.is_dir())
        .unwrap_or(false);
    judge(&sec, &user, is_dir, process_is_elevated()).map_err(denied)
}

/// Make `path` owner-only if it is not already; returns whether a change was
/// needed. Use on load to tighten a file an older build or another tool left
/// with a broader list.
pub fn ensure_owner_only(path: &Path, is_dir: bool) -> io::Result<bool> {
    if verify_owner_only(path).is_ok() {
        return Ok(false);
    }
    restrict_to_owner(path, is_dir)?;
    verify_owner_only(path).map_err(|e| explain(path, e))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "awili-winacl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_default_file_is_not_owner_only() {
        let dir = temp_dir();
        let f = dir.join("plain.txt");
        std::fs::write(&f, "x").unwrap();
        assert!(verify_owner_only(&f).is_err(), "inherited ACL must fail");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn restrict_file_reads_back_as_user_and_system_only() {
        let dir = temp_dir();
        let f = dir.join("secret.toml");
        std::fs::write(&f, "key = 1").unwrap();
        restrict_to_owner(&f, false).unwrap();
        verify_owner_only(&f).unwrap();
        let (protected, grants) = read_dacl(&f).unwrap();
        assert!(protected);
        assert_eq!(grants.len(), 2, "{grants:?}");
        let user = current_user_sid_string().unwrap();
        assert!(grants.iter().any(|g| g.sid == user));
        assert!(grants.iter().any(|g| g.sid == "S-1-5-18"));
        // The owner can still read and rewrite it.
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "key = 1");
        std::fs::write(&f, "key = 2").unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn restrict_folder_is_inherited_by_new_files() {
        let dir = temp_dir();
        let sub = dir.join("private");
        std::fs::create_dir(&sub).unwrap();
        restrict_to_owner(&sub, true).unwrap();
        verify_owner_only(&sub).unwrap();
        let child = sub.join("db.sqlite");
        std::fs::write(&child, "x").unwrap();
        let (protected, grants) = read_dacl(&child).unwrap();
        assert!(!protected, "a child inherits; it is not itself protected");
        let user = current_user_sid_string().unwrap();
        assert!(
            grants.iter().all(|g| g.sid == user || g.sid == "S-1-5-18"),
            "{grants:?}"
        );
        assert!(!grants.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn sec(user: &str, flags: u8, owner: &str) -> Security {
        let g = |sid: &str| Grant {
            sid: sid.into(),
            mask: FILE_ALL_ACCESS,
            flags,
        };
        Security {
            protected: true,
            grants: vec![g(user), g(SYSTEM_SID)],
            owner: owner.into(),
        }
    }

    #[test]
    fn judge_requires_exactly_what_restrict_builds() {
        let u = "S-1-5-21-1-2-3-1001";
        assert!(judge(&sec(u, 0, u), u, false, false).is_ok());
        assert!(judge(&sec(u, OI_CI, u), u, true, false).is_ok());
        // Folder entries must inherit; file entries carry no flags.
        assert!(judge(&sec(u, 0, u), u, true, false).is_err());
        assert!(judge(&sec(u, OI_CI, u), u, false, false).is_err());
        // Not protected, a third principal, a missing principal, a weak mask.
        let mut s = sec(u, 0, u);
        s.protected = false;
        assert!(judge(&s, u, false, false).is_err());
        let mut s = sec(u, 0, u);
        s.grants.push(Grant {
            sid: "S-1-1-0".into(),
            mask: FILE_ALL_ACCESS,
            flags: 0,
        });
        assert!(judge(&s, u, false, false).is_err());
        let mut s = sec(u, 0, u);
        s.grants.truncate(1);
        assert!(judge(&s, u, false, false).is_err());
        let mut s = sec(u, 0, u);
        s.grants[0].mask = 0x0012_00A9;
        assert!(judge(&s, u, false, false).is_err());
    }

    #[test]
    fn judge_owner_rules() {
        let u = "S-1-5-21-1-2-3-1001";
        // Someone else owns it: refused even when elevated.
        assert!(judge(&sec(u, 0, "S-1-5-21-9-9-9-500"), u, false, true).is_err());
        // Administrators own it: fine only for an elevated process.
        assert!(judge(&sec(u, 0, ADMINS_SID), u, false, true).is_ok());
        let e = judge(&sec(u, 0, ADMINS_SID), u, false, false).unwrap_err();
        assert!(e.contains("not by you"), "{e}");
    }

    #[test]
    fn created_file_is_owner_only_from_the_start() {
        let dir = temp_dir();
        let f = dir.join("new.toml");
        let mut file = create_new_file_owner_only(&f).unwrap();
        verify_owner_only(&f).unwrap();
        std::io::Write::write_all(&mut file, b"key = 1").unwrap();
        drop(file);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "key = 1");
        // A second create of the same name is refused, not truncated.
        let e = create_new_file_owner_only(&f).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn created_dir_is_owner_only_and_inherited() {
        let dir = temp_dir();
        let d = dir.join("private");
        create_dir_owner_only(&d).unwrap();
        verify_owner_only(&d).unwrap();
        let (_, grants) = read_dacl(&d).unwrap();
        assert!(grants.iter().all(|g| g.flags == OI_CI), "{grants:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_read_back_removes_what_was_created() {
        let dir = temp_dir();
        let f = dir.join("seamed.toml");
        let d = dir.join("seamed");
        seam::force_verify_failure(true);
        assert!(create_new_file_owner_only(&f).is_err());
        assert!(create_dir_owner_only(&d).is_err());
        seam::force_verify_failure(false);
        assert!(!f.exists() && !d.exists(), "nothing is left behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn this_volume_reports_file_permissions() {
        assert_eq!(volume_supports_acls(&std::env::temp_dir()), Some(true));
        // A folder that does not exist yet is judged by its nearest parent.
        let nope = std::env::temp_dir().join("a").join("b").join("c");
        assert_eq!(volume_supports_acls(&nope), Some(true));
    }

    #[test]
    fn ensure_tightens_once() {
        let dir = temp_dir();
        let f = dir.join("c.toml");
        std::fs::write(&f, "x").unwrap();
        assert!(ensure_owner_only(&f, false).unwrap());
        assert!(!ensure_owner_only(&f, false).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
