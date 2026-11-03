//! awili-engine-sandbox — C3: filesystem operations that CANNOT escape the
//! authorized session root.
//!
//! Rewrite per the C-phase arbitration (the 2026-07-26 implementation was
//! REWRITE-verdicted: it canonicalized a pathname and later opened by
//! pathname, so `O_NOFOLLOW` protected only the final component and a raced
//! parent symlink could redirect the operation; its write path also
//! post-checked AFTER the bytes landed).
//!
//! # The mechanism
//!
//! The root is opened ONCE as a directory descriptor at construction. Every
//! subsequent operation resolves **component by component, descriptor
//! relative, with no-follow semantics** — `openat(dirfd, component,
//! O_NOFOLLOW | O_DIRECTORY)` walking down, then the leaf. A symlink
//! anywhere in the path — including a parent swapped mid-walk — fails the
//! open with `ELOOP` before any side effect, because we never re-derive a
//! pathname and never hand a multi-component path to the kernel.
//!
//! This closes the parent-component TOCTOU race that pathname validation
//! structurally cannot: there is no window between "check" and "use",
//! because the check IS the use — each descriptor is obtained from the
//! previous descriptor, and the chain is anchored at the root fd.
//!
//! # Layering
//!
//! C1 authorizes (policy over strings) and stamps `scope_root` into the
//! evidence; C2's runtime hands that root here; C3 enforces at execution.
//! C1's textual screening is UX, this is the security boundary — contract
//! v2 §6.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Component, Path};

use awili_engine_log::authz::AuthzEvidence;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// RC2-ENG6 (T19): the authorization handed to the sandbox does not
    /// cover this call (or names no folder) — refused, but not a path
    /// escape.
    #[error("not authorized: {0}")]
    NotAuthorized(String),
    #[error("path escapes the session root: {0}")]
    Escape(String),
    #[error("symlink encountered at {0} (no-follow resolution)")]
    Symlink(String),
    #[error("multiply-linked file at {0} (hardlink containment)")]
    Hardlink(String),
    #[error("not a regular file: {0}")]
    NotRegularFile(String),
    #[error("invalid path component in {0}")]
    InvalidComponent(String),
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// A bounded operation exceeded its declared limit (C2-u4). NOT a
    /// containment refusal — but still a failed dispatch: over-limit
    /// results are refused whole, never truncated (a truncated read is
    /// a lie the model would act on).
    #[error("limit exceeded: {0}")]
    TooLarge(String),
}

impl SandboxError {
    /// Containment-class refusals (C2-u4 design finding 5): errors that
    /// mean the sandbox REFUSED something for a security reason, as
    /// opposed to ordinary tree conditions (unreadable file, fifo) or
    /// resource limits. During tree-walking operations (grep), a
    /// containment-class refusal fails the WHOLE dispatch; ordinary
    /// conditions are skipped-and-reported.
    pub fn is_containment(&self) -> bool {
        matches!(
            self,
            SandboxError::Escape(_)
                | SandboxError::NotAuthorized(_)
                | SandboxError::Symlink(_)
                | SandboxError::Hardlink(_)
                | SandboxError::InvalidComponent(_)
        )
    }
}

/// Resource limits for the builtin-facing bounded operations (C2-u4
/// design finding 4). Enforced WHILE reading/walking — before unbounded
/// allocation — never by truncating an already-built result.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Max bytes read from (or written to) a single file.
    pub max_file_bytes: usize,
    /// Max directory entries visited in one walk (glob/grep).
    pub max_entries: usize,
    /// Max grep matches accumulated before the search is refused whole.
    pub max_matches: usize,
    /// Max total result bytes a bounded operation may ACCUMULATE (glob
    /// path bytes, grep matched-line bytes). Enforced before every
    /// append — a result is refused whole, never truncated.
    pub max_result_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 1 << 20, // 1 MiB
            max_entries: 10_000,
            max_matches: 2_000,
            max_result_bytes: 1 << 20, // 1 MiB
        }
    }
}

/// A filesystem sandbox anchored at an open directory descriptor.
pub struct Sandbox {
    root: OwnedFd,
    root_display: String,
    /// The path the root was opened by. Only used to turn an absolute path
    /// that names a place inside the root back into a relative one (RC1-O:
    /// models are told the folder's full path and then use it); the walk
    /// itself stays anchored at `root`.
    root_path: std::path::PathBuf,
    /// The root's mount (device id plus mount identity). Every resolved
    /// component must share it — closes mount/bind-mount crossing (Codex C3
    /// P0): a mount beneath the root otherwise opens normally and reaches
    /// another namespace. RC1-P: the device id alone cannot tell a
    /// same-device bind mount (or macOS nullfs mount) from the root's own
    /// tree; the mount identity can.
    root_mount: MountIdent,
}

/// RC1-P: where a descriptor lives — its device id and, when the platform
/// reports it, the identity of the mount it is on (macOS: `fstatfs`
/// `f_fsid` together with the mount-on path `f_mntonname` — RC1-P-FIX1,
/// pre-review P2: the path alone is not an identity; Linux: `statx`
/// `stx_mnt_id`, kernel 5.8+). A
/// kernel that cannot report a mount id leaves `mount` empty and the check
/// falls back to the device id alone (the pre-RC1-P behaviour).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MountIdent {
    dev: u64,
    mount: Option<MountKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MountKey {
    /// macOS: the filesystem id and the path the mount is mounted on; two
    /// descriptors are on one mount only when both agree.
    #[cfg(target_os = "macos")]
    Fs { fsid: [i32; 2], on: Vec<u8> },
    /// Linux: the kernel's mount id.
    #[cfg(target_os = "linux")]
    Id(u64),
}

impl MountIdent {
    /// Same device, and the same mount whenever both sides report one.
    pub(crate) fn same_mount(&self, other: &MountIdent) -> bool {
        self.dev == other.dev
            && match (&self.mount, &other.mount) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
    }
}

/// RC1-P: the mount identity of an open descriptor (see [`MountIdent`]).
fn mount_ident(fd: libc::c_int) -> Result<MountIdent, SandboxError> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fd is an open descriptor owned by the caller; st is a valid
    // out-pointer.
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(SandboxError::Io(io::Error::last_os_error()));
    }
    Ok(MountIdent {
        dev: st.st_dev as u64,
        mount: mount_key(fd)?,
    })
}

#[cfg(target_os = "macos")]
fn mount_key(fd: libc::c_int) -> Result<Option<MountKey>, SandboxError> {
    let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: fd is open; sfs is a valid out-pointer.
    if unsafe { libc::fstatfs(fd, &mut sfs) } != 0 {
        return Err(SandboxError::Io(io::Error::last_os_error()));
    }
    // SAFETY: the kernel fills f_mntonname with a NUL-terminated path inside
    // the fixed-size array.
    let on = unsafe { std::ffi::CStr::from_ptr(sfs.f_mntonname.as_ptr()) };
    // SAFETY: `fsid_t` is `#[repr(C)] { [i32; 2] }` (Darwin `struct fsid
    // { int32_t val[2]; }`); libc keeps the field private, so it is read as
    // the array it is. Same size, checked by `transmute` at compile time.
    let fsid: [i32; 2] = unsafe { std::mem::transmute::<libc::fsid_t, [i32; 2]>(sfs.f_fsid) };
    Ok(Some(MountKey::Fs {
        fsid,
        on: on.to_bytes().to_vec(),
    }))
}

/// RC2-ENG6 (S-P2): the mount id from `/proc/self/fdinfo/<fd>`'s `mnt_id:`
/// line (Linux 3.15+) — the same id `statx` reports — for a kernel without
/// `statx`'s `STATX_MNT_ID` or a seccomp filter that refuses `statx`. Only
/// when this is unavailable too does the check fall back to the device id
/// alone.
#[cfg(target_os = "linux")]
fn fdinfo_mount_id(fd: libc::c_int) -> Option<u64> {
    #[cfg(test)]
    if kernel_says::NO_FDINFO.with(std::cell::Cell::get) {
        return None;
    }
    let text = std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).ok()?;
    parse_fdinfo_mount_id(&text)
}

/// RC2-ENG6 (S-P2): the `mnt_id:` value of an fdinfo text.
#[cfg(any(target_os = "linux", test))]
fn parse_fdinfo_mount_id(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix("mnt_id:"))
        .and_then(|v| v.trim().parse().ok())
}

/// RC1-P-FIX2: errors that mean `statx` itself cannot be used here — the
/// kernel lacks it (`ENOSYS`) or a seccomp filter refuses it (`EPERM`, as
/// some container runtimes return) — rather than a failure on this
/// descriptor.
#[cfg(target_os = "linux")]
fn statx_unavailable(errno: Option<i32>) -> bool {
    matches!(errno, Some(libc::ENOSYS) | Some(libc::EPERM))
}

/// RC2-ENGB3 (S-P1): test-only stand-ins for what the kernel answers, so a
/// test can drive `open_root` and an ordinary read through a refused
/// `statx` and a missing fdinfo (a gate machine's kernel answers neither
/// way on demand). Per thread: each test runs on its own.
#[cfg(all(test, target_os = "linux"))]
mod kernel_says {
    use std::cell::Cell;
    thread_local! {
        /// `statx` fails with this errno.
        pub(super) static STATX_ERRNO: Cell<Option<i32>> = const { Cell::new(None) };
        /// `/proc/self/fdinfo` gives no mount id.
        pub(super) static NO_FDINFO: Cell<bool> = const { Cell::new(false) };
        /// RC2-ENGF1: `statx` succeeds but reports no mount id (a kernel
        /// before 5.8).
        pub(super) static STATX_NO_MNT_ID: Cell<bool> = const { Cell::new(false) };
    }
}

/// `statx` for the mount id; the error when it fails.
#[cfg(target_os = "linux")]
fn statx_mount_id(fd: libc::c_int, stx: &mut libc::statx) -> Option<io::Error> {
    #[cfg(test)]
    if let Some(errno) = kernel_says::STATX_ERRNO.with(std::cell::Cell::get) {
        return Some(io::Error::from_raw_os_error(errno));
    }
    // SAFETY: fd is open; the empty path with AT_EMPTY_PATH names fd itself;
    // stx is a valid out-pointer.
    let rc = unsafe {
        libc::statx(
            fd,
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_MNT_ID,
            stx,
        )
    };
    #[cfg(test)]
    if rc == 0 && kernel_says::STATX_NO_MNT_ID.with(std::cell::Cell::get) {
        stx.stx_mask &= !libc::STATX_MNT_ID;
    }
    (rc != 0).then(io::Error::last_os_error)
}

#[cfg(target_os = "linux")]
fn mount_key(fd: libc::c_int) -> Result<Option<MountKey>, SandboxError> {
    let mut stx: libc::statx = unsafe { std::mem::zeroed() };
    if let Some(e) = statx_mount_id(fd, &mut stx) {
        // A kernel without statx, or a seccomp filter that refuses it: no
        // mount id, device id only (RC1-P-FIX1 slice review P2: refusing the
        // folder outright there would be a regression).
        if statx_unavailable(e.raw_os_error()) {
            return Ok(fdinfo_mount_id(fd).map(MountKey::Id));
        }
        return Err(SandboxError::Io(e));
    }
    if stx.stx_mask & libc::STATX_MNT_ID == 0 {
        return Ok(fdinfo_mount_id(fd).map(MountKey::Id));
    }
    Ok(Some(MountKey::Id(stx.stx_mnt_id)))
}

/// What a resolution yields: the parent directory descriptor plus the leaf
/// name, so the final operation uses `*at` syscalls that never see a
/// multi-component path. CRATE-PRIVATE (Codex C3 P0): handing a raw
/// directory fd to callers would let them `openat(parent, "../outside")`
/// and escape — the sandbox's own methods are the only API.
pub(crate) struct Resolved {
    parent: OwnedFd,
    leaf: CString,
    leaf_display: String,
}

impl Sandbox {
    /// Open the authorized root once. Everything else hangs off this fd —
    /// even if the path is renamed afterwards, operations stay anchored to
    /// the directory that was authorized.
    pub fn open_root(root: &Path) -> Result<Self, SandboxError> {
        // Byte-exact path (Codex C3 P1: to_string_lossy could anchor a
        // DIFFERENT directory than the authorized one for non-UTF-8 names).
        use std::os::unix::ffi::OsStrExt;
        let c = CString::new(root.as_os_str().as_bytes())
            .map_err(|_| SandboxError::InvalidComponent(root.display().to_string()))?;
        // SAFETY: valid CString; flags are constants; we own the returned fd.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(SandboxError::Io(io::Error::last_os_error()));
        }
        let root_fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let root_mount = mount_ident(root_fd.as_raw_fd())?;
        Ok(Self {
            root: root_fd,
            root_display: root.display().to_string(),
            root_path: root.to_path_buf(),
            root_mount,
        })
    }

    pub fn root_display(&self) -> &str {
        &self.root_display
    }

    /// Resolve a path descriptor-by-descriptor beneath the root.
    ///
    /// An absolute path whose leading components are exactly the root's own
    /// path (component-wise, so `/x/work-evil` is not under `/x/work`) is
    /// taken as the relative path that follows (RC1-O); the walk is the same
    /// descriptor walk, so a symlink inside still cannot lead out.
    ///
    /// Rejected structurally (never by string inspection):
    /// - any other absolute path and `~` (no anchor but the root fd is permitted),
    /// - any `..` component (there is no legitimate use inside a scoped
    ///   session, and allowing it would require re-deriving pathnames),
    /// - a symlink at ANY component, including one swapped mid-walk.
    pub(crate) fn resolve(&self, rel: &str) -> Result<Resolved, SandboxError> {
        let mut path = Path::new(rel);
        if rel.starts_with('~') {
            return Err(SandboxError::Escape(rel.to_string()));
        }
        if path.is_absolute() {
            match path.strip_prefix(&self.root_path) {
                Ok(inner) if !inner.as_os_str().is_empty() => path = inner,
                Ok(_) => return Err(SandboxError::InvalidComponent(rel.to_string())),
                Err(_) => return Err(SandboxError::Escape(rel.to_string())),
            }
        }
        let mut comps: Vec<String> = Vec::new();
        for c in path.components() {
            match c {
                Component::Normal(s) => comps.push(s.to_string_lossy().into_owned()),
                Component::CurDir => {}
                Component::ParentDir => return Err(SandboxError::Escape(rel.to_string())),
                Component::RootDir | Component::Prefix(_) => {
                    return Err(SandboxError::Escape(rel.to_string()))
                }
            }
        }
        let Some(leaf) = comps.pop() else {
            return Err(SandboxError::InvalidComponent(rel.to_string()));
        };

        // Walk the parents, each open relative to the previous descriptor.
        let mut cur = dup_fd(&self.root)?;
        for c in &comps {
            cur = openat_dir_nofollow(&cur, c, rel)?;
            // Mount containment: a component on a different device is a
            // mount point — refuse rather than walk into another tree.
            if !self.root_mount.same_mount(&mount_ident(cur.as_raw_fd())?) {
                return Err(SandboxError::Escape(format!(
                    "{rel}: component crosses a mount point"
                )));
            }
        }
        Ok(Resolved {
            parent: cur,
            leaf: cstr(&leaf)?,
            leaf_display: rel.to_string(),
        })
    }

    /// Read a file inside the sandbox. The leaf is opened `O_NOFOLLOW`
    /// (and `O_NONBLOCK` — S1(b), F0612: a FIFO leaf must not block the
    /// open before the regular-file check can reject it) from its resolved
    /// parent descriptor.
    pub fn read(&self, rel: &str) -> Result<Vec<u8>, SandboxError> {
        let r = self.resolve(rel)?;
        let fd = unsafe {
            libc::openat(
                r.parent.as_raw_fd(),
                r.leaf.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let err = io::Error::last_os_error();
            return Err(classify_refusal(err, &r.parent, &r.leaf, &r.leaf_display));
        }
        let f = unsafe { std::fs::File::from_raw_fd(fd) };
        check_leaf_containment(&f, &self.root_mount, &r.leaf_display)?;
        let mut f = f;
        use std::io::Read;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// [`Sandbox::read`] with a byte limit enforced WHILE reading: at most
    /// `limits.max_file_bytes + 1` bytes are ever pulled, and exceeding the
    /// limit refuses the whole read with [`SandboxError::TooLarge`]. The
    /// leaf open carries `O_NONBLOCK` for the same FIFO reason as
    /// [`Sandbox::read`].
    pub fn read_bounded(&self, rel: &str, limits: &Limits) -> Result<Vec<u8>, SandboxError> {
        let r = self.resolve(rel)?;
        let fd = unsafe {
            libc::openat(
                r.parent.as_raw_fd(),
                r.leaf.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let err = io::Error::last_os_error();
            return Err(classify_refusal(err, &r.parent, &r.leaf, &r.leaf_display));
        }
        let f = unsafe { std::fs::File::from_raw_fd(fd) };
        check_leaf_containment(&f, &self.root_mount, &r.leaf_display)?;
        use std::io::Read;
        let cap = limits.max_file_bytes as u64;
        let mut buf = Vec::new();
        let mut limited = f.take(cap + 1);
        limited.read_to_end(&mut buf)?;
        if buf.len() as u64 > cap {
            return Err(SandboxError::TooLarge(format!(
                "{rel}: file exceeds {} bytes",
                limits.max_file_bytes
            )));
        }
        Ok(buf)
    }

    /// [`Sandbox::write`] with the contents length checked against
    /// `limits.max_file_bytes` BEFORE any filesystem side effect.
    pub fn write_bounded(
        &self,
        rel: &str,
        contents: &[u8],
        limits: &Limits,
    ) -> Result<(), SandboxError> {
        if contents.len() > limits.max_file_bytes {
            return Err(SandboxError::TooLarge(format!(
                "{rel}: contents exceed {} bytes",
                limits.max_file_bytes
            )));
        }
        self.write(rel, contents)
    }

    /// Write a file inside the sandbox. `O_NOFOLLOW` means an existing
    /// symlink at the leaf fails the OPEN — no bytes are written and no
    /// post-hoc check is needed (the Set A implementation wrote first and
    /// checked after; that was its second P0). `O_NONBLOCK` (S1(b), F0612)
    /// means a FIFO leaf cannot make the open wait for a reader; the
    /// non-regular refusal below fires before any truncate or write.
    pub fn write(&self, rel: &str, contents: &[u8]) -> Result<(), SandboxError> {
        let r = self.resolve(rel)?;
        // NO O_TRUNC on the open (self-review: truncation is a SIDE EFFECT
        // and must not precede the containment check — the exact bug class
        // this rewrite exists to eliminate). Open, verify, THEN ftruncate.
        let fd = unsafe {
            libc::openat(
                r.parent.as_raw_fd(),
                r.leaf.as_ptr(),
                libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
                0o644 as libc::c_uint,
            )
        };
        if fd < 0 {
            let err = io::Error::last_os_error();
            return Err(classify_refusal(err, &r.parent, &r.leaf, &r.leaf_display));
        }
        let f = unsafe { std::fs::File::from_raw_fd(fd) };
        check_leaf_containment(&f, &self.root_mount, &r.leaf_display)?;
        // Containment proven on this very descriptor; now truncate + write.
        if unsafe { libc::ftruncate(f.as_raw_fd(), 0) } != 0 {
            return Err(SandboxError::Io(io::Error::last_os_error()));
        }
        let mut f = f;
        use std::io::Write;
        f.write_all(contents)?;
        Ok(())
    }

    /// Existence check that follows the same resolution rules.
    /// RC2-ENG6 (S-P3): a leaf that is itself on another mount (a mount
    /// point, or a file bind-mounted into the folder) is refused like a
    /// read of it — it is not reported as an entry of the folder.
    pub fn exists(&self, rel: &str) -> Result<bool, SandboxError> {
        let r = self.resolve(rel)?;
        match self.leaf_on_root_mount(&r)? {
            None => Ok(false),
            Some(true) => Ok(true),
            Some(false) => Err(SandboxError::Escape(format!(
                "{rel}: crosses a mount point"
            ))),
        }
    }

    /// RC2-ENG6 (S-P3): whether a resolved leaf is on the root's mount —
    /// `None` when it does not exist. The device id comes from a no-follow
    /// `fstatat`; the mount identity is then read without needing read
    /// permission on the leaf (RC2-ENGF1: an unreadable leaf was judged by
    /// its device id alone, so an unreadable same-device bind mount looked
    /// like an ordinary entry of the folder).
    fn leaf_on_root_mount(&self, r: &Resolved) -> Result<Option<bool>, SandboxError> {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: parent is an open directory descriptor; leaf is a valid C
        // string; st is a valid out-pointer.
        let rc = unsafe {
            libc::fstatat(
                r.parent.as_raw_fd(),
                r.leaf.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return Ok(None);
        }
        self.leaf_mount_matches(r, &st)
    }

    /// RC2-ENGF1 (Linux): every leaf, of any kind and whatever its
    /// permissions, is opened `O_PATH | O_NOFOLLOW` — which needs no read
    /// permission and has no effect on the file (a FIFO or device is not
    /// opened) — and judged by its full mount identity.
    #[cfg(target_os = "linux")]
    fn leaf_mount_matches(
        &self,
        r: &Resolved,
        _st: &libc::stat,
    ) -> Result<Option<bool>, SandboxError> {
        // SAFETY: parent is an open directory descriptor; leaf is a valid C
        // string.
        let fd = unsafe {
            libc::openat(
                r.parent.as_raw_fd(),
                r.leaf.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENOENT) {
                return Ok(None); // gone since the fstatat
            }
            return Err(SandboxError::Io(err));
        }
        // SAFETY: openat returned a fresh descriptor we now own.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Some(
            self.root_mount.same_mount(&mount_ident(fd.as_raw_fd())?),
        ))
    }

    /// RC2-ENGF1 (macOS): a regular file or directory that opens
    /// `O_NOFOLLOW | O_NONBLOCK` is judged by its full mount identity, as
    /// before. Anything else — no read permission, or a kind whose open
    /// could have effects — is judged by what macOS reports without opening
    /// it (`getattrlistat`): on the root's mount only when its device and
    /// filesystem id are the root's and it is not itself a mount point
    /// (macOS mounts only on directories).
    #[cfg(target_os = "macos")]
    fn leaf_mount_matches(
        &self,
        r: &Resolved,
        st: &libc::stat,
    ) -> Result<Option<bool>, SandboxError> {
        let kind = st.st_mode & libc::S_IFMT;
        if kind == libc::S_IFREG || kind == libc::S_IFDIR {
            // SAFETY: as in `leaf_on_root_mount`; the descriptor is owned
            // below.
            let fd = unsafe {
                libc::openat(
                    r.parent.as_raw_fd(),
                    r.leaf.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if fd >= 0 {
                // SAFETY: openat returned a fresh descriptor we now own.
                let fd = unsafe { OwnedFd::from_raw_fd(fd) };
                return Ok(Some(
                    self.root_mount.same_mount(&mount_ident(fd.as_raw_fd())?),
                ));
            }
        }
        let Some(MountKey::Fs {
            fsid: root_fsid, ..
        }) = &self.root_mount.mount
        else {
            return Err(SandboxError::Escape(format!(
                "{}: cannot tell which mount it is on",
                r.leaf_display
            )));
        };
        let Some((fsid, mount_point)) = unopened_leaf_attrs(r, kind == libc::S_IFDIR)? else {
            return Ok(None); // gone since the fstatat
        };
        Ok(Some(
            st.st_dev as u64 == self.root_mount.dev && fsid == *root_fsid && !mount_point,
        ))
    }
}

/// RC2-ENGF1 (macOS): a leaf's filesystem id and, for a directory, whether
/// something is mounted on it — read with `getattrlistat`, which needs no
/// permission on the leaf itself. `None` when the leaf is gone.
#[cfg(target_os = "macos")]
fn unopened_leaf_attrs(
    r: &Resolved,
    is_dir: bool,
) -> Result<Option<([i32; 2], bool)>, SandboxError> {
    /// The reply layout for `ATTR_CMN_FSID` (+ `ATTR_DIR_MOUNTSTATUS`):
    /// every attribute is 4-byte aligned, so `repr(C)` matches it.
    #[repr(C)]
    struct Reply {
        length: u32,
        fsid: [i32; 2],
        mount_status: u32,
    }
    // SAFETY: attrlist is plain data; zero is a valid starting value.
    let mut want: libc::attrlist = unsafe { std::mem::zeroed() };
    want.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    want.commonattr = libc::ATTR_CMN_FSID;
    if is_dir {
        want.dirattr = libc::ATTR_DIR_MOUNTSTATUS;
    }
    let mut reply = Reply {
        length: 0,
        fsid: [0; 2],
        mount_status: 0,
    };
    // SAFETY: parent is an open directory descriptor; leaf is a valid C
    // string; `want` and `reply` are valid for the sizes given.
    let rc = unsafe {
        libc::getattrlistat(
            r.parent.as_raw_fd(),
            r.leaf.as_ptr(),
            (&mut want as *mut libc::attrlist).cast(),
            (&mut reply as *mut Reply).cast(),
            std::mem::size_of::<Reply>(),
            libc::FSOPT_NOFOLLOW as _,
        )
    };
    if rc != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(SandboxError::Io(err));
    }
    let wanted = std::mem::size_of::<u32>()
        + std::mem::size_of::<[i32; 2]>()
        + if is_dir {
            std::mem::size_of::<u32>()
        } else {
            0
        };
    if (reply.length as usize) < wanted {
        return Err(SandboxError::Escape(format!(
            "{}: cannot tell which mount it is on",
            r.leaf_display
        )));
    }
    Ok(Some((
        reply.fsid,
        is_dir && reply.mount_status & libc::DIR_MNTSTATUS_MNTPOINT != 0,
    )))
}

/// (device, inode) identity of an open descriptor — cycle detection.
#[allow(clippy::unnecessary_cast)] // st_dev/st_ino widths differ per-OS; casts are portable
fn fstat_ident(fd: &OwnedFd) -> Result<(u64, u64), SandboxError> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(fd.as_raw_fd(), &mut st) };
    if rc != 0 {
        return Err(SandboxError::Io(io::Error::last_os_error()));
    }
    Ok((st.st_dev as u64, st.st_ino as u64))
}

/// Leaf containment (Codex C3 P0): same mount as the root (RC1-P: device id
/// and mount identity — a file bind-mounted over a path inside the root is
/// on another mount), and — for
/// regular files — no additional hard links. A hardlink placed inside the
/// root pointing at outside content has st_nlink > 1; refusing multiply-
/// linked regular files closes that disclosure/overwrite channel.
fn check_leaf_containment(
    f: &std::fs::File,
    root_mount: &MountIdent,
    path: &str,
) -> Result<(), SandboxError> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(f.as_raw_fd(), &mut st) };
    if rc != 0 {
        return Err(SandboxError::Io(io::Error::last_os_error()));
    }
    if !root_mount.same_mount(&mount_ident(f.as_raw_fd())?) {
        return Err(SandboxError::Escape(format!(
            "{path}: crosses a mount point"
        )));
    }
    // Leaf type restriction (Codex C3 P1): only regular files. Device
    // nodes, FIFOs, sockets and other special files are refused outright —
    // same-device special files would otherwise pass the dev/nlink checks
    // and reach resources outside the tree.
    if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return Err(SandboxError::NotRegularFile(path.to_string()));
    }
    if st.st_nlink > 1 {
        return Err(SandboxError::Hardlink(path.to_string()));
    }
    Ok(())
}

fn cstr(s: &str) -> Result<CString, SandboxError> {
    CString::new(s).map_err(|_| SandboxError::InvalidComponent(s.to_string()))
}

fn dup_fd(fd: &OwnedFd) -> Result<OwnedFd, SandboxError> {
    // F_DUPFD_CLOEXEC, not dup (Codex C3 P1): descriptors must not leak
    // across exec once C3 gains the shell path.
    let n = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if n < 0 {
        return Err(SandboxError::Io(io::Error::last_os_error()));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(n) })
}

fn openat_dir_nofollow(parent: &OwnedFd, name: &str, whole: &str) -> Result<OwnedFd, SandboxError> {
    let c = cstr(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let err = io::Error::last_os_error();
        return Err(classify_refusal(err, parent, &c, whole));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Classify a refused open. The refusal itself is the security property —
/// this only decides which error the caller sees. Platforms disagree on the
/// errno for "O_NOFOLLOW hit a symlink": Linux gives ELOOP, macOS gives
/// ENOTDIR when O_DIRECTORY is also set. Rather than guess from errno, ASK
/// the filesystem (fstatat + AT_SYMLINK_NOFOLLOW, which never follows) so
/// the classification is exact on both.
fn classify_refusal(e: io::Error, parent: &OwnedFd, name: &CString, path: &str) -> SandboxError {
    if matches!(
        e.raw_os_error(),
        Some(libc::ELOOP) | Some(libc::EMLINK) | Some(libc::ENOTDIR)
    ) && lstat_is_symlink(parent, name)
    {
        return SandboxError::Symlink(path.to_string());
    }
    SandboxError::Io(e)
}

fn lstat_is_symlink(parent: &OwnedFd, name: &CString) -> bool {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    rc == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFLNK
}

/// Open the sandbox from an authorization's scope root — the C1→C2→C3
/// handshake (contract v2 §6). C2's runtime calls this with the evidence
/// it received alongside the ticket; a tool call with no authorized root
/// gets no filesystem sandbox at all.
pub fn sandbox_for_evidence(
    evidence: &AuthzEvidence,
    turn_id: &str,
    call: &awili_engine::events::ToolCall,
) -> Result<Sandbox, SandboxError> {
    // Type-enforced AND call-bound handshake (Codex C3 P0 x2): the root
    // comes from C1's authorization value, and that value must authorize
    // THIS call — evidence replayed from another call is refused here as
    // well as at intent().
    if !evidence.authorizes(turn_id, call) {
        return Err(SandboxError::NotAuthorized(
            "authorization does not cover this call".into(),
        ));
    }
    let Some(root) = evidence.scope_root() else {
        return Err(SandboxError::NotAuthorized(
            "authorization carries no scope root".into(),
        ));
    };
    Sandbox::open_root(Path::new(root))
}

#[cfg(test)]
mod handshake_tests {
    use super::*;

    /// The C1→C3 handshake is type-enforced: the root comes from the
    /// authorization value, and an authorization without one yields no
    /// sandbox. (Constructing evidence requires C1's gate, which is why
    /// this lives in-crate against a gate-minted value.)
    #[test]
    fn evidence_without_scope_root_yields_no_sandbox() {
        use awili_engine::events::{AwiliCallId, ToolCall};
        use awili_engine_log::authz::RiskClass;
        // The shared fixture (unit 4 r7) owns this test process's sole
        // issuance — the former `let Ok(..) else return` tolerance for an
        // already-issued gate is unnecessary and gone.
        let bundle = awili_engine_log::test_bundle();
        let gate = &bundle.0;
        let call = ToolCall {
            awili_call_id: AwiliCallId("c".into()),
            provider_call_id: None,
            name: "fs.read".into(),
            arguments: "{}".into(),
        };
        let ev = gate
            .policy_evidence(
                &bundle.2.mint(awili_engine::ToolIdentity::new(
                    awili_engine::ToolOrigin::new("builtin:fs"),
                    "fs.read",
                    awili_engine::DescriptorDigest([0u8; 32]),
                )),
                "t",
                &call,
                RiskClass::ReadOnly,
                None,
            )
            .unwrap();
        assert!(matches!(
            sandbox_for_evidence(&ev, "t", &call),
            Err(SandboxError::NotAuthorized(_))
        ));

        let d = tempfile::tempdir().unwrap();
        let ev2 = gate
            .policy_evidence(
                &bundle.2.mint(awili_engine::ToolIdentity::new(
                    awili_engine::ToolOrigin::new("builtin:fs"),
                    "fs.read",
                    awili_engine::DescriptorDigest([0u8; 32]),
                )),
                "t",
                &call,
                RiskClass::ReadOnly,
                Some(d.path().to_str().unwrap()),
            )
            .unwrap();
        assert!(sandbox_for_evidence(&ev2, "t", &call).is_ok());

        // Evidence minted for THIS call must not open a sandbox for another.
        let other = ToolCall {
            awili_call_id: AwiliCallId("other".into()),
            provider_call_id: None,
            name: "fs.write".into(),
            arguments: "{}".into(),
        };
        assert!(matches!(
            sandbox_for_evidence(&ev2, "t", &other),
            Err(SandboxError::NotAuthorized(_))
        ));
    }

    /// RC2-ENG6 (T19): an authorization that names no folder, or that was
    /// minted for another call, is refused as NOT AUTHORIZED — no longer
    /// reported as a path escaping the folder.
    #[test]
    fn rc2eng6_an_unauthorized_call_is_not_called_a_path_escape() {
        use awili_engine::events::{AwiliCallId, ToolCall};
        use awili_engine_log::authz::RiskClass;
        let bundle = awili_engine_log::test_bundle();
        let call = ToolCall {
            awili_call_id: AwiliCallId("c-t19".into()),
            provider_call_id: None,
            name: "fs.read".into(),
            arguments: "{}".into(),
        };
        let mint = |root: Option<&str>| {
            bundle
                .0
                .policy_evidence(
                    &bundle.2.mint(awili_engine::ToolIdentity::new(
                        awili_engine::ToolOrigin::new("builtin:fs"),
                        "fs.read",
                        awili_engine::DescriptorDigest([0u8; 32]),
                    )),
                    "t",
                    &call,
                    RiskClass::ReadOnly,
                    root,
                )
                .unwrap()
        };
        let d = tempfile::tempdir().unwrap();
        let other = ToolCall {
            awili_call_id: AwiliCallId("c-other".into()),
            ..call.clone()
        };
        for err in [
            sandbox_for_evidence(&mint(None), "t", &call).err().unwrap(),
            sandbox_for_evidence(&mint(d.path().to_str()), "t", &other)
                .err()
                .unwrap(),
        ] {
            assert!(matches!(err, SandboxError::NotAuthorized(_)), "{err:?}");
            let text = err.to_string();
            assert!(text.starts_with("not authorized"), "{text}");
            assert!(!text.contains("escape"), "{text}");
            assert!(err.is_containment(), "still a security refusal");
        }
    }
}

#[cfg(test)]
mod fifo_tests {
    //! S1(b) proof test (F0612): a FIFO leaf must not make the leaf `openat`
    //! block before the regular-file check. `read`, `read_bounded` and
    //! `write` each return a non-regular refusal promptly with NO reader
    //! (or writer) present, and a write to a FIFO must not truncate or
    //! modify it. Ordinary regular files still work.

    use super::*;
    use std::os::unix::fs::OpenOptionsExt;
    use std::time::Duration;

    /// Create a FIFO at `path` (mkfifo(3)).
    fn make_fifo(path: &std::path::Path) {
        use std::os::unix::ffi::OsStrExt;
        let c = CString::new(path.as_os_str().as_bytes()).expect("fifo path is NUL-free");
        assert_eq!(
            unsafe { libc::mkfifo(c.as_ptr(), 0o644 as libc::mode_t) },
            0
        );
    }

    /// Run `f` on a spawned thread and wait at most 2s for the result
    /// (RVS1B-FIX1, F2): a regression that BLOCKS must fail the test with a
    /// timeout panic instead of hanging the test run. `f` opens its own
    /// `Sandbox` (cloning the root `PathBuf`) — a `Sandbox` is not `Sync`
    /// and must not be shared across threads.
    fn within<T: Send + 'static + std::fmt::Debug>(
        _root: std::path::PathBuf,
        what: &str,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (tx, rx) = std::sync::mpsc::channel::<Result<T, ()>>();
        std::thread::spawn(move || {
            let r = f();
            let _ = tx.send(Ok(r));
        });
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(v)) => v,
            other => panic!("{what} did not return within 2s: {other:?}"),
        }
    }

    #[test]
    fn sandbox_fifo_read_and_write_return_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        make_fifo(dir.path().join("pipe").as_path());
        let limits = Limits::default();

        // Every leaf operation must return the non-regular refusal —
        // promptly (a blocking open would stall the test process) and
        // without a reader or writer on the other end. Each operation runs
        // through `within` so a blocking regression FAILS here instead of
        // hanging the whole test run.
        let got = within(root.clone(), "sb.read(\"pipe\")", {
            let root = root.clone();
            move || Sandbox::open_root(&root).unwrap().read("pipe")
        });
        assert!(matches!(got, Err(SandboxError::NotRegularFile(_))));
        let got = within(root.clone(), "sb.read_bounded(\"pipe\", &limits)", {
            let root = root.clone();
            move || {
                Sandbox::open_root(&root)
                    .unwrap()
                    .read_bounded("pipe", &limits)
            }
        });
        assert!(matches!(got, Err(SandboxError::NotRegularFile(_))));
        // The write open returns ENXIO (no reader) BEFORE the regular-file
        // check can fire: that exact `Io` error is the open-level refusal,
        // the `NotRegularFile` refusal is the fstat-level one; no other
        // `Io` error may pass, and neither may block or modify the FIFO.
        let got = within(root.clone(), "sb.write(\"pipe\", b\"payload\")", {
            let root = root.clone();
            move || Sandbox::open_root(&root).unwrap().write("pipe", b"payload")
        });
        assert!(
            matches!(got, Err(SandboxError::NotRegularFile(_)))
                || matches!(&got, Err(SandboxError::Io(e)) if e.raw_os_error() == Some(libc::ENXIO)),
            "write to FIFO with no reader must be refused with NotRegularFile or ENXIO, got: {got:?}"
        );

        // A write refusal must not have created or modified any regular
        // file: the FIFO is still the only entry and is still a FIFO.
        let sb = Sandbox::open_root(&root).unwrap();
        let entries = sb.list_dir("").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].rel, "pipe");
        assert!(!entries[0].is_dir);
        use std::os::unix::fs::FileTypeExt;
        let st = std::fs::metadata(dir.path().join("pipe")).unwrap();
        assert!(st.file_type().is_fifo());

        // Ordinary regular files still work end to end.
        sb.write("plain.txt", b"ok").unwrap();
        assert_eq!(sb.read("plain.txt").unwrap(), b"ok");
        assert_eq!(sb.read_bounded("plain.txt", &limits).unwrap(), b"ok");
    }

    #[test]
    fn sandbox_fifo_with_peer_is_refused_without_transfer() {
        // RVS1B-FIX1 (F2): with a NON-BLOCKING reader open on the other end,
        // the sandbox leaf opens SUCCEED (no ENXIO) and the refusal must come
        // from `check_leaf_containment`'s fstat check on the OPENED FIFO —
        // and none of the refused operations may transfer or consume data.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        make_fifo(dir.path().join("pipe").as_path());
        let limits = Limits::default();

        // Peer reader: non-blocking, so the open returns even with no writer.
        let mut reader = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(dir.path().join("pipe"))
            .unwrap();
        // Peer writer: succeeds precisely because a reader now exists.
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(dir.path().join("pipe"))
            .unwrap();
        use std::io::Write;
        writer.write_all(b"held").unwrap();

        // Opened-FIFO refusal on the READ side: the open succeeds, the
        // refusal is the fstat check — exactly `NotRegularFile`, promptly.
        let got = within(root.clone(), "sb.read(\"pipe\") with peer reader", {
            let root = root.clone();
            move || Sandbox::open_root(&root).unwrap().read("pipe")
        });
        assert!(
            matches!(got, Err(SandboxError::NotRegularFile(_))),
            "read with peer must be exactly NotRegularFile, got: {got:?}"
        );
        let got = within(
            root.clone(),
            "sb.read_bounded(\"pipe\", &limits) with peer reader",
            {
                let root = root.clone();
                move || {
                    Sandbox::open_root(&root)
                        .unwrap()
                        .read_bounded("pipe", &limits)
                }
            },
        );
        assert!(
            matches!(got, Err(SandboxError::NotRegularFile(_))),
            "read_bounded with peer must be exactly NotRegularFile, got: {got:?}"
        );
        // Opened-FIFO refusal on the WRITE side: with the reader present the
        // open succeeds, so this is the regular-file refusal on an opened
        // FIFO — exactly `NotRegularFile`, no `Io` alternative.
        let got = within(
            root.clone(),
            "sb.write(\"pipe\", b\"payload\") with peer reader",
            {
                let root = root.clone();
                move || Sandbox::open_root(&root).unwrap().write("pipe", b"payload")
            },
        );
        assert!(
            matches!(got, Err(SandboxError::NotRegularFile(_))),
            "write with peer must be exactly NotRegularFile, got: {got:?}"
        );

        // The sandbox reads consumed nothing and the refused write sent no
        // bytes: dropping the writer closes the pipe, and the held bytes are
        // exactly what the reader still holds.
        drop(writer);
        use std::io::Read;
        let mut got = Vec::new();
        reader.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"held");

        // The refused operations left the FIFO intact: still a FIFO, still
        // the only entry, and no extra regular file was created.
        let st = std::fs::symlink_metadata(dir.path().join("pipe")).unwrap();
        use std::os::unix::fs::FileTypeExt;
        assert!(st.file_type().is_fifo());
        let sb = Sandbox::open_root(&root).unwrap();
        let entries = sb.list_dir("").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].rel, "pipe");
        assert!(!entries[0].is_dir);
    }
}

// # Residual limits — stated, not papered over
//
// Review findings this layer cannot fully close are carried here rather
// than claiming more than the code does:
//
// - **Same-device mounts — closed by RC1-P.** `st_dev` alone let a bind
//   mount (or macOS nullfs mount) that keeps the source device id pass.
//   Every walked parent, `open_dir` and every leaf now also compare the
//   mount identity (`MountIdent`: Linux `statx` `stx_mnt_id`, macOS
//   `fstatfs` `f_fsid` + `f_mntonname`). Residual: a Linux kernel older
//   than 5.8 (no `STATX_MNT_ID`) falls back to the device id alone, the
//   behaviour before RC1-P; `openat2(RESOLVE_NO_XDEV)` would close that
//   too and is not used here.
// - **`st_nlink` is a point-in-time check.** It proves the leaf was not
//   multiply-linked at open; another process can link the inode
//   afterwards. That channel lets an attacker who already has write
//   access inside the root observe what WE write — it does not disclose
//   outside content nor redirect our writes. Closing it fully requires
//   filesystem-level isolation (a dedicated dataset/namespace per
//   session), which is a fleet decision, not a code fix.

// SCOPE NOTE (updated 2026-08-06): read/write/exists/edit/list/glob/grep
// are implemented here, all on the same `resolve` primitive. The remaining
// C3 surface is SHELL execution routed through the existing terminal layer
// (C3c) — it must receive the root descriptor, not a pathname, and nothing
// may bypass `Sandbox` to implement it.

// ---------------------------------------------------------------------------
// C3b: edit / list / glob / grep — all on the SAME resolution primitive.
// Nothing here re-derives a pathname or opens by multi-component path.
// ---------------------------------------------------------------------------

/// One entry from a sandboxed directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Path relative to the sandbox root, as the caller would pass it back.
    pub rel: String,
    pub is_dir: bool,
}

/// Search result: hits PLUS every entry that could not be read and why —
/// so a containment refusal is visible rather than silently absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepReport {
    pub matches: Vec<Match>,
    /// (relative path, reason) for entries skipped during the search.
    pub skipped: Vec<(String, String)>,
}

/// A grep hit: 1-based line number and the matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub rel: String,
    pub line_no: usize,
    pub line: String,
}

impl Sandbox {
    /// Read-modify-write within the sandbox, on RAW BYTES (Codex C3b P1:
    /// lossy UTF-8 conversion would rewrite unrelated bytes and destroy
    /// binaries). Honest limitation: this is TWO resolutions, read then
    /// write. Both are O_NOFOLLOW + containment-checked, so a swap between
    /// them cannot escape the root — the residual is a lost-update race
    /// against a concurrent in-root writer, which is a correctness
    /// property, not a containment one.
    pub fn edit(&self, rel: &str, find: &[u8], replace: &[u8]) -> Result<usize, SandboxError> {
        if find.is_empty() {
            return Err(SandboxError::InvalidComponent("empty find pattern".into()));
        }
        let before = self.read(rel)?;
        let mut out = Vec::with_capacity(before.len());
        let mut i = 0usize;
        let mut count = 0usize;
        while i < before.len() {
            if before[i..].starts_with(find) {
                out.extend_from_slice(replace);
                i += find.len();
                count += 1;
            } else {
                out.push(before[i]);
                i += 1;
            }
        }
        if count == 0 {
            return Ok(0);
        }
        self.write(rel, &out)?;
        Ok(count)
    }

    /// [`Sandbox::edit`] with both the read and the resulting write bounded
    /// by `limits.max_file_bytes` (the read is enforced while reading; the
    /// replacement result is checked before any write side effect).
    pub fn edit_bounded(
        &self,
        rel: &str,
        find: &[u8],
        replace: &[u8],
        limits: &Limits,
    ) -> Result<usize, SandboxError> {
        if find.is_empty() {
            return Err(SandboxError::InvalidComponent("empty find pattern".into()));
        }
        let before = self.read_bounded(rel, limits)?;
        let mut out = Vec::with_capacity(before.len().min(limits.max_file_bytes));
        let mut i = 0usize;
        let mut count = 0usize;
        // Replacement amplification guard (C2-u4 round-1 P1): the result
        // must stay writable under the same limit, checked BEFORE each
        // append — a bounded input with a large replacement must not
        // allocate past the bound before the final write check.
        while i < before.len() {
            if before[i..].starts_with(find) {
                if out.len() + replace.len() > limits.max_file_bytes {
                    return Err(SandboxError::TooLarge(format!(
                        "{rel}: edit result exceeds {} bytes",
                        limits.max_file_bytes
                    )));
                }
                out.extend_from_slice(replace);
                i += find.len();
                count += 1;
            } else {
                if out.len() + 1 > limits.max_file_bytes {
                    return Err(SandboxError::TooLarge(format!(
                        "{rel}: edit result exceeds {} bytes",
                        limits.max_file_bytes
                    )));
                }
                out.push(before[i]);
                i += 1;
            }
        }
        if count == 0 {
            return Ok(0);
        }
        self.write_bounded(rel, &out, limits)?;
        Ok(count)
    }

    /// List a directory INSIDE the sandbox. Resolution rules identical to
    /// read/write: symlinked or mount-crossing components refuse.
    pub fn list_dir(&self, rel: &str) -> Result<Vec<DirEntry>, SandboxError> {
        let dir_fd = self.open_dir(rel)?;
        read_dir_fd(&dir_fd, rel)
    }

    /// Recursive walk bounded by the sandbox, with a simple `*` glob on the
    /// FINAL component (e.g. `*.md`, `notes*`). Symlinked subdirectories are
    /// skipped, never followed — a walk cannot leave the root.
    pub fn glob(&self, pattern: &str) -> Result<Vec<String>, SandboxError> {
        let (dir_part, pat) = match pattern.rsplit_once('/') {
            Some((d, p)) => (d.to_string(), p.to_string()),
            None => (String::new(), pattern.to_string()),
        };
        let mut out = Vec::new();
        self.walk(&dir_part, &mut |e: &DirEntry| {
            if !e.is_dir {
                let leaf = e.rel.rsplit('/').next().unwrap_or(&e.rel);
                if glob_match(&pat, leaf) {
                    out.push(e.rel.clone());
                }
            }
        })?;
        self.drop_mounted_leaves(&mut out)?;
        out.sort();
        Ok(out)
    }

    /// [`Sandbox::glob`] with an entry budget enforced DURING the walk:
    /// visiting more than `limits.max_entries` directory entries refuses
    /// the whole operation with [`SandboxError::TooLarge`].
    pub fn glob_bounded(
        &self,
        pattern: &str,
        limits: &Limits,
    ) -> Result<Vec<String>, SandboxError> {
        let (dir_part, pat) = match pattern.rsplit_once('/') {
            Some((d, p)) => (d.to_string(), p.to_string()),
            None => (String::new(), pattern.to_string()),
        };
        let mut out = Vec::new();
        let mut budget = limits.max_entries;
        let mut result_bytes = 0usize;
        self.walk_try(&dir_part, &mut |e: &DirEntry| {
            if budget == 0 {
                return Err(SandboxError::TooLarge(format!(
                    "{pattern}: more than {} entries",
                    limits.max_entries
                )));
            }
            budget -= 1;
            if !e.is_dir {
                let leaf = e.rel.rsplit('/').next().unwrap_or(&e.rel);
                if glob_match(&pat, leaf) {
                    // Result-byte cap BEFORE the append (round-1 P1).
                    result_bytes += e.rel.len();
                    if result_bytes > limits.max_result_bytes {
                        return Err(SandboxError::TooLarge(format!(
                            "{pattern}: result exceeds {} bytes",
                            limits.max_result_bytes
                        )));
                    }
                    out.push(e.rel.clone());
                }
            }
            Ok(())
        })?;
        self.drop_mounted_leaves(&mut out)?;
        out.sort();
        Ok(out)
    }

    /// RC2-ENG6 (S-P3): a glob names only files on the root's own mount —
    /// a file bind-mounted into the folder is left out (never named as the
    /// folder's), as a read of it is refused.
    fn drop_mounted_leaves(&self, names: &mut Vec<String>) -> Result<(), SandboxError> {
        let mut kept = Vec::with_capacity(names.len());
        for rel in names.drain(..) {
            let r = self.resolve(&rel)?;
            if self.leaf_on_root_mount(&r)? == Some(true) {
                kept.push(rel);
            }
        }
        *names = kept;
        Ok(())
    }

    /// Fixed-string search across the sandboxed tree (optionally scoped to a
    /// subdirectory). Binary-ish files are skipped by lossy decoding.
    pub fn grep(&self, needle: &str, under: &str) -> Result<GrepReport, SandboxError> {
        if needle.is_empty() {
            return Err(SandboxError::InvalidComponent("empty search".into()));
        }
        let mut files = Vec::new();
        self.walk(under, &mut |e: &DirEntry| {
            if !e.is_dir {
                files.push(e.rel.clone());
            }
        })?;
        files.sort();
        let mut out = Vec::new();
        let mut skipped: Vec<(String, String)> = Vec::new();
        for rel in files {
            // Nothing is silently omitted (Codex C3b P1) — but a symlink
            // or special file in a real tree is normal, not an attack, so
            // aborting the whole search would be wrong too. Unreadable
            // entries are SKIPPED AND REPORTED, with the reason.
            let bytes = match self.read(&rel) {
                Ok(b) => b,
                Err(e) => {
                    skipped.push((rel.clone(), e.to_string()));
                    continue;
                }
            };
            for (i, line) in String::from_utf8_lossy(&bytes).lines().enumerate() {
                if line.contains(needle) {
                    out.push(Match {
                        rel: rel.clone(),
                        line_no: i + 1,
                        line: line.to_string(),
                    });
                }
            }
        }
        Ok(GrepReport {
            matches: out,
            skipped,
        })
    }

    /// [`Sandbox::grep`] with C2-u4 semantics (design findings 4+5):
    ///
    /// - entry budget and match cap enforced while walking/matching —
    ///   over-limit refuses the WHOLE search ([`SandboxError::TooLarge`]);
    /// - a CONTAINMENT-class refusal on any entry (symlink, hardlink,
    ///   escape, invalid component — see [`SandboxError::is_containment`])
    ///   fails the whole dispatch: the plain [`Sandbox::grep`] reports such
    ///   entries as skips, which would let a builtin dispatch return
    ///   `ok=true` while C3 had refused something — refusals must not be
    ///   laundered into skip lines;
    /// - ordinary tree conditions (unreadable file, fifo, per-file
    ///   over-limit) remain skipped-and-reported.
    pub fn grep_bounded(
        &self,
        needle: &str,
        under: &str,
        limits: &Limits,
    ) -> Result<GrepReport, SandboxError> {
        self.grep_bounded_where(needle, under, limits, &|_| true)
    }

    /// RC2-R2G1: the files beneath `under` (sorted), with the entry budget
    /// of [`Sandbox::grep_bounded`]'s walk: the files a search of `under`
    /// reads.
    pub fn files_bounded(&self, under: &str, limits: &Limits) -> Result<Vec<String>, SandboxError> {
        let mut files = Vec::new();
        let mut budget = limits.max_entries;
        self.walk_try(under, &mut |e: &DirEntry| {
            if budget == 0 {
                return Err(SandboxError::TooLarge(format!(
                    "{under}: more than {} entries",
                    limits.max_entries
                )));
            }
            budget -= 1;
            if !e.is_dir {
                files.push(e.rel.clone());
            }
            Ok(())
        })?;
        files.sort();
        Ok(files)
    }

    /// RC2-R2G1: [`Sandbox::grep_bounded`] over only the files `keep`
    /// accepts; the others are never opened (a secret file the owner did
    /// not allow).
    pub fn grep_bounded_where(
        &self,
        needle: &str,
        under: &str,
        limits: &Limits,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<GrepReport, SandboxError> {
        if needle.is_empty() {
            return Err(SandboxError::InvalidComponent("empty search".into()));
        }
        let mut files = self.files_bounded(under, limits)?;
        files.retain(|rel| keep(rel));
        let mut out = Vec::new();
        let mut skipped: Vec<(String, String)> = Vec::new();
        let mut result_bytes = 0usize;
        for rel in files {
            let bytes = match self.read_bounded(&rel, limits) {
                Ok(b) => b,
                // Containment refusal: fail the whole dispatch, loudly.
                Err(e) if e.is_containment() => return Err(e),
                // Ordinary tree condition: skipped AND reported — and
                // CHARGED against the result cap like any other output
                // (round-2 P1: max_entries long skip records must not
                // allocate past max_result_bytes).
                Err(e) => {
                    let why = e.to_string();
                    result_bytes += rel.len() + why.len();
                    if result_bytes > limits.max_result_bytes {
                        return Err(SandboxError::TooLarge(format!(
                            "grep result exceeds {} bytes",
                            limits.max_result_bytes
                        )));
                    }
                    skipped.push((rel.clone(), why));
                    continue;
                }
            };
            for (i, line) in String::from_utf8_lossy(&bytes).lines().enumerate() {
                if line.contains(needle) {
                    if out.len() >= limits.max_matches {
                        return Err(SandboxError::TooLarge(format!(
                            "more than {} matches",
                            limits.max_matches
                        )));
                    }
                    // Result-byte cap BEFORE the append (round-1 P1): a
                    // few thousand long matching lines must not allocate
                    // unbounded output.
                    result_bytes += rel.len() + line.len();
                    if result_bytes > limits.max_result_bytes {
                        return Err(SandboxError::TooLarge(format!(
                            "grep result exceeds {} bytes",
                            limits.max_result_bytes
                        )));
                    }
                    out.push(Match {
                        rel: rel.clone(),
                        line_no: i + 1,
                        line: line.to_string(),
                    });
                }
            }
        }
        Ok(GrepReport {
            matches: out,
            skipped,
        })
    }

    /// Open a directory inside the sandbox (empty string = the root itself).
    fn open_dir(&self, rel: &str) -> Result<OwnedFd, SandboxError> {
        if rel.is_empty() || rel == "." || Path::new(rel) == self.root_path {
            return dup_fd(&self.root);
        }
        let r = self.resolve(rel)?;
        let fd = openat_dir_nofollow(&r.parent, leaf_str(&r)?.as_str(), rel)?;
        if !self.root_mount.same_mount(&mount_ident(fd.as_raw_fd())?) {
            return Err(SandboxError::Escape(format!(
                "{rel}: crosses a mount point"
            )));
        }
        Ok(fd)
    }

    fn walk(&self, rel: &str, f: &mut dyn FnMut(&DirEntry)) -> Result<(), SandboxError> {
        self.walk_try(rel, &mut |e| {
            f(e);
            Ok(())
        })
    }

    /// Walk with a FALLIBLE callback (C2-u4): entry budgets and match caps
    /// are enforced inside the callback, which aborts the walk by
    /// returning the error — bounded work, not bounded-result-after-
    /// unbounded-work.
    fn walk_try(
        &self,
        rel: &str,
        f: &mut dyn FnMut(&DirEntry) -> Result<(), SandboxError>,
    ) -> Result<(), SandboxError> {
        // Bounded traversal (Codex C3b P1: the "cannot cycle" claim was
        // false — bind mounts and future hardlinked dirs can cycle, and a
        // deep tree can blow the stack). Depth cap + visited inode set.
        let mut visited = std::collections::BTreeSet::new();
        self.walk_bounded(rel, f, 0, &mut visited)
    }

    fn walk_bounded(
        &self,
        rel: &str,
        f: &mut dyn FnMut(&DirEntry) -> Result<(), SandboxError>,
        depth: usize,
        visited: &mut std::collections::BTreeSet<(u64, u64)>,
    ) -> Result<(), SandboxError> {
        const MAX_DEPTH: usize = 64;
        if depth > MAX_DEPTH {
            return Err(SandboxError::Escape(format!(
                "{rel}: traversal depth limit ({MAX_DEPTH}) exceeded"
            )));
        }
        let dir_fd = self.open_dir(rel)?;
        let ident = fstat_ident(&dir_fd)?;
        if !visited.insert(ident) {
            return Ok(()); // already walked this inode: cycle, stop.
        }
        // STREAMING (C2-u4 round-1 P1): the fallible callback runs as
        // readdir yields, so an entry budget aborts before a huge
        // directory is materialized. Subdirectories are collected for
        // recursion AFTER passing the callback — their number is bounded
        // by whatever budget the callback enforces.
        let mut subdirs = Vec::new();
        read_dir_fd_each(&dir_fd, rel, &mut |e| {
            f(&e)?;
            if e.is_dir {
                subdirs.push(e.rel);
            }
            Ok(())
        })?;
        for sub in subdirs {
            self.walk_bounded(&sub, f, depth + 1, visited)?;
        }
        Ok(())
    }
}

/// Portable errno pointer. The libc crate exposes different symbols per
/// platform (Darwin `__error`, Linux `__errno_location`) — cfg-gated so
/// this crate builds on the Mac dev target AND the Linux fleet.
/// (Self-inflicted bug caught here: an earlier "portable" version called
/// close(-1), which SETS errno to EBADF and made every readdir look like
/// a failure.)
#[cfg(target_os = "macos")]
unsafe fn errno_ptr() -> *mut libc::c_int {
    libc::__error()
}

#[cfg(target_os = "linux")]
unsafe fn errno_ptr() -> *mut libc::c_int {
    libc::__errno_location()
}

fn clear_errno() {
    unsafe { *errno_ptr() = 0 };
}

fn errno_now() -> libc::c_int {
    unsafe { *errno_ptr() }
}

fn stat_is_dir(parent: &OwnedFd, name: &std::ffi::CStr) -> bool {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    rc == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFDIR
}

fn leaf_str(r: &Resolved) -> Result<String, SandboxError> {
    r.leaf
        .to_str()
        .map(|s| s.to_string())
        .map_err(|_| SandboxError::InvalidComponent(r.leaf_display.clone()))
}

/// `*` matches any run of characters; everything else is literal.
fn glob_match(pat: &str, name: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == name;
    }
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !name.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            return name[pos..].ends_with(part) && name.len() >= pos + part.len();
        } else if let Some(idx) = name[pos..].find(part) {
            pos += idx + part.len();
        } else {
            return false;
        }
    }
    true
}

fn read_dir_fd(dir_fd: &OwnedFd, rel_prefix: &str) -> Result<Vec<DirEntry>, SandboxError> {
    let mut out = Vec::new();
    read_dir_fd_each(dir_fd, rel_prefix, &mut |e| {
        out.push(e);
        Ok(())
    })?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// STREAMING directory enumeration (C2-u4 round-1 P1): the callback runs
/// as `readdir` yields each entry, so an entry budget aborts BEFORE a
/// huge directory is materialized. `read_dir_fd` (list_dir and the plain
/// unbudgeted walk) collects and sorts on top of this.
fn read_dir_fd_each(
    dir_fd: &OwnedFd,
    rel_prefix: &str,
    f: &mut dyn FnMut(DirEntry) -> Result<(), SandboxError>,
) -> Result<(), SandboxError> {
    // fdopendir takes ownership of the fd; dup so our OwnedFd stays valid.
    let dup = dup_fd(dir_fd)?;
    let raw = dup.as_raw_fd();
    std::mem::forget(dup); // ownership moves to the DIR*
    let dirp = unsafe { libc::fdopendir(raw) };
    if dirp.is_null() {
        let e = io::Error::last_os_error();
        unsafe { libc::close(raw) };
        return Err(SandboxError::Io(e));
    }
    // REWIND (bug found by the second-listing test): dup'd descriptors
    // SHARE their file offset, so a directory read leaves the shared
    // offset at EOF and every later listing of the same directory returns
    // empty. rewinddir resets the stream before we walk it.
    unsafe { libc::rewinddir(dirp) };
    loop {
        // Portable errno handling (Codex C3b P1: libc::__error() is Darwin
        // only; the fleet is Linux). set_errno via a fresh Error is not
        // available, so we clear by calling a syscall that succeeds, then
        // read errno through std::io::Error::last_os_error() after readdir
        // returns null — nonzero means a real mid-stream failure, not EOF.
        clear_errno();
        let ent = unsafe { libc::readdir(dirp) };
        if ent.is_null() {
            let err = errno_now();
            if err != 0 {
                unsafe { libc::closedir(dirp) };
                return Err(SandboxError::Io(io::Error::from_raw_os_error(err)));
            }
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) };
        let Ok(name) = name.to_str() else { continue };
        if name == "." || name == ".." {
            continue;
        }
        // d_type may be DT_UNKNOWN on some filesystems (Codex C3b P1):
        // fall back to a no-follow stat rather than mislabeling dirs as
        // files and silently truncating glob/grep results.
        let dt = unsafe { (*ent).d_type };
        let is_dir = if dt == libc::DT_UNKNOWN {
            let c = match std::ffi::CString::new(name) {
                Ok(c) => c,
                Err(_) => {
                    unsafe { libc::closedir(dirp) };
                    return Err(SandboxError::InvalidComponent(name.to_string()));
                }
            };
            stat_is_dir(dir_fd, &c)
        } else {
            dt == libc::DT_DIR
        };
        let rel = if rel_prefix.is_empty() || rel_prefix == "." {
            name.to_string()
        } else {
            format!("{}/{}", rel_prefix.trim_end_matches('/'), name)
        };
        if let Err(e) = f(DirEntry { rel, is_dir }) {
            unsafe { libc::closedir(dirp) };
            return Err(e);
        }
    }
    unsafe { libc::closedir(dirp) };
    Ok(())
}

#[cfg(test)]
mod rc1p_tests {
    use super::*;

    #[test]
    fn rc1p_same_mount_needs_the_same_device_and_mount() {
        #[cfg(target_os = "macos")]
        let (k1, k2) = (
            MountKey::Fs {
                fsid: [1, 2],
                on: b"/".to_vec(),
            },
            MountKey::Fs {
                fsid: [1, 3],
                on: b"/".to_vec(),
            },
        );
        #[cfg(target_os = "linux")]
        let (k1, k2) = (MountKey::Id(1), MountKey::Id(2));
        let at = |dev, mount| MountIdent { dev, mount };
        assert!(at(1, Some(k1.clone())).same_mount(&at(1, Some(k1.clone()))));
        // Same device, another mount: a same-device bind or nullfs mount.
        assert!(!at(1, Some(k1.clone())).same_mount(&at(1, Some(k2.clone()))));
        // Another device is another mount whatever the key says.
        assert!(!at(1, Some(k1.clone())).same_mount(&at(2, Some(k1.clone()))));
        // A kernel that cannot report a mount id: device id alone.
        assert!(at(1, None).same_mount(&at(1, Some(k2.clone()))));
        assert!(!at(1, None).same_mount(&at(2, None)));
    }

    /// RC1-P-FIX2: a kernel without `statx` or a seccomp refusal falls back
    /// to the device id; any other error is still an error.
    #[cfg(target_os = "linux")]
    #[test]
    fn rc1p_fix2_statx_refusal_falls_back_to_the_device_id() {
        assert!(statx_unavailable(Some(libc::ENOSYS)));
        assert!(statx_unavailable(Some(libc::EPERM)));
        assert!(!statx_unavailable(Some(libc::EBADF)));
        assert!(!statx_unavailable(Some(libc::EACCES)));
        assert!(!statx_unavailable(None));
    }

    /// A directory and its subdirectory on one filesystem share the mount.
    #[test]
    fn rc1p_a_plain_subdirectory_is_the_same_mount() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let root = std::fs::File::open(dir.path()).unwrap();
        let sub = std::fs::File::open(dir.path().join("sub")).unwrap();
        let a = mount_ident(root.as_raw_fd()).unwrap();
        let b = mount_ident(sub.as_raw_fd()).unwrap();
        assert!(a.same_mount(&b), "{a:?} {b:?}");
        // Every supported platform reports a mount identity here.
        assert!(a.mount.is_some(), "{a:?}");
    }
}

#[cfg(test)]
mod rc2_eng6_tests {
    use super::*;

    /// RC2-ENG6 (S-P2): the fdinfo fallback reads the `mnt_id:` line.
    #[test]
    fn rc2eng6_the_fdinfo_mount_id_is_read() {
        let text = "pos:\t0\nflags:\t0100000\nmnt_id:\t29\nino:\t1234\n";
        assert_eq!(parse_fdinfo_mount_id(text), Some(29));
        assert_eq!(parse_fdinfo_mount_id("pos:\t0\n"), None);
        assert_eq!(parse_fdinfo_mount_id("mnt_id:\tx\n"), None);
    }

    /// RC2-ENG6 (S-P2): on Linux the fallback reports the same mount id as
    /// `statx`, so a root and a leaf judged by different paths still agree.
    /// RC2-ENGF1 (RC2-ENG slice review P3: the test compared fdinfo with a
    /// WORKING `statx` and never entered the fallback): `mount_key` itself
    /// is driven into both fallback branches — `statx` refused, and `statx`
    /// without a mount id — and must report statx's own id from fdinfo;
    /// with no fdinfo either, no mount id at all.
    #[cfg(target_os = "linux")]
    #[test]
    fn rc2eng6_the_fdinfo_mount_id_matches_statx() {
        fn set(errno: Option<i32>, no_mnt_id: bool, no_fdinfo: bool) {
            kernel_says::STATX_ERRNO.with(|c| c.set(errno));
            kernel_says::STATX_NO_MNT_ID.with(|c| c.set(no_mnt_id));
            kernel_says::NO_FDINFO.with(|c| c.set(no_fdinfo));
        }
        let dir = tempfile::tempdir().unwrap();
        let f = std::fs::File::open(dir.path()).unwrap();
        let Some(MountKey::Id(by_statx)) = mount_key(f.as_raw_fd()).unwrap() else {
            panic!("statx reports a mount id on the gate");
        };
        assert_eq!(fdinfo_mount_id(f.as_raw_fd()), Some(by_statx));
        for (errno, no_mnt_id) in [(Some(libc::ENOSYS), false), (None, true)] {
            set(errno, no_mnt_id, false);
            let fell_back = mount_key(f.as_raw_fd());
            set(errno, no_mnt_id, true);
            let nothing = mount_key(f.as_raw_fd());
            set(None, false, false);
            assert_eq!(
                fell_back.unwrap(),
                Some(MountKey::Id(by_statx)),
                "{errno:?} {no_mnt_id}: the fdinfo fallback"
            );
            assert_eq!(
                nothing.unwrap(),
                None,
                "{errno:?} {no_mnt_id}: device id only"
            );
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod rc2_engb3_tests {
    use super::*;

    /// Sets what the kernel says for this thread; back to the real answers
    /// when dropped.
    struct KernelSays;
    impl KernelSays {
        fn statx_fails(errno: i32, no_fdinfo: bool) -> Self {
            kernel_says::STATX_ERRNO.with(|c| c.set(Some(errno)));
            kernel_says::NO_FDINFO.with(|c| c.set(no_fdinfo));
            KernelSays
        }
    }
    impl Drop for KernelSays {
        fn drop(&mut self) {
            kernel_says::STATX_ERRNO.with(|c| c.set(None));
            kernel_says::NO_FDINFO.with(|c| c.set(false));
        }
    }

    /// RC2-ENGB3 (S-P1, RC1-P slice review P3): with `statx` refused
    /// (`EPERM`, a seccomp filter; `ENOSYS`, an old kernel), `open_root` still
    /// opens the folder and an ordinary read works — on the fdinfo mount id
    /// when /proc gives it, else on the device id alone. Any other `statx`
    /// error still refuses the folder. (Before RC1-P-FIX2 an `EPERM` failed
    /// `open_root`; the FIX2 test checked only the predicate.)
    #[test]
    fn rc2engb3_a_refused_statx_still_opens_the_folder_and_reads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hi").unwrap();
        let real = {
            let f = std::fs::File::open(dir.path()).unwrap();
            fdinfo_mount_id(f.as_raw_fd()).expect("fdinfo on the gate")
        };

        for errno in [libc::EPERM, libc::ENOSYS] {
            {
                let _k = KernelSays::statx_fails(errno, false);
                let s = Sandbox::open_root(dir.path()).expect("opens on the fdinfo id");
                assert_eq!(s.root_mount.mount, Some(MountKey::Id(real)), "{errno}");
                assert_eq!(s.read("a.txt").unwrap(), b"hi");
            }
            {
                let _k = KernelSays::statx_fails(errno, true);
                let s = Sandbox::open_root(dir.path()).expect("opens on the device id");
                assert_eq!(s.root_mount.mount, None, "{errno}: device id alone");
                assert_eq!(s.read("a.txt").unwrap(), b"hi");
                assert!(s.exists("a.txt").unwrap());
            }
        }
        {
            let _k = KernelSays::statx_fails(libc::EACCES, false);
            assert!(
                matches!(Sandbox::open_root(dir.path()), Err(SandboxError::Io(_))),
                "any other statx error still refuses the folder"
            );
        }
        // The real kernel again: statx's own mount id.
        let s = Sandbox::open_root(dir.path()).unwrap();
        assert_eq!(s.root_mount.mount, Some(MountKey::Id(real)));
    }
}
