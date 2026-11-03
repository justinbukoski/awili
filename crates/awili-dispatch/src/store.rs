//! Atomic snapshots and process-scoped OS locks. No stale lock files to reap.
//! S3(a): the job root is opened once and every job directory, control
//! directory and leaf is reached through it, one path component per call,
//! with no-follow semantics, so swapped links and directories are never
//! followed on either Linux or macOS.
use crate::protocol::*;
use anyhow::{ensure, Result};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::fs::{fcntl_getfl, fcntl_setfl, fsync, mkdirat, open, openat, renameat, Mode, OFlags};
use rustix::io::Errno;
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// Name of the per-request control directory inside the job directory.
pub const CONTROL_DIR: &str = "control";
/// Name of the record leaf inside the control directory.
pub const RECORD_LEAF: &str = "record.json";
/// Name of the global lock leaf at the job root.
pub const LOCK_LEAF: &str = ".lock";
/// Name of the worker log leaf inside the control directory.
pub const LOG_LEAF: &str = "worker.log";
/// Name of the branch reservation directory at the job root.
pub const BRANCHES_DIR: &str = "branches";
/// Name of the provenance leaf inside the job directory.
pub const SPEC_LEAF: &str = "spec.txt";

/// Error from the store's leaf helpers: a caller-supplied name that failed
/// validation, or an open/`fstat` failure on the leaf itself. `BadName` is
/// produced before any `openat` is issued.
#[derive(Debug, PartialEq)]
pub enum StoreError {
    /// The path component failed `check_name`; nothing was opened.
    BadName,
    /// `openat` or `fstat` on the leaf failed.
    Open(String),
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadName => write!(f, "bad path component"),
            Self::Open(msg) => write!(f, "{msg}"),
        }
    }
}
impl std::error::Error for StoreError {}

pub struct Store {
    pub root: PathBuf,
    root_fd: OwnedFd,
}
impl Store {
    /// Open a job root: absolute, created with 0700, and kept open as the
    /// anchor every later openat call resolves from. The root path is
    /// canonicalized ONCE here (the administrator-configured root is
    /// trusted, so its own symlinked ancestors — e.g. macOS `/var` →
    /// `/private/var` — are resolved at this single step only); from the
    /// anchor down, every open stays no-follow.
    pub fn open(root: &Path) -> Result<Self> {
        ensure!(root.is_absolute(), "job root must be absolute");
        fs::create_dir_all(root)?;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        // canonicalize follows symlinks in the ROOT PATH ITSELF (and its
        // ancestors) — the one place following is allowed. It also proves
        // the path exists and resolves to a real directory.
        let canonical = root.canonicalize()?;
        let root_fd = open(
            &canonical,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Self {
            root: canonical,
            root_fd,
        })
    }
    /// Refuse multi-component, dotted or empty path components before any
    /// openat call: every helper opens exactly one component. Also refuses
    /// NUL bytes so the OS-level EILSEQ check is not the only line of
    /// defense.
    fn check_name(name: &str) -> Result<()> {
        ensure!(
            !name.is_empty()
                && name != "."
                && name != ".."
                && !name.contains('/')
                && !name.contains('\0'),
            "bad path component"
        );
        Ok(())
    }
    /// Open a subdirectory of `dir` by name, no-follow, read-only.
    fn open_dir_at(dir: BorrowedFd<'_>, name: &str) -> Result<OwnedFd> {
        Self::check_name(name)?;
        openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|code| anyhow::anyhow!("cannot open directory {name}: {code}"))
    }
    /// Reject a leaf descriptor that is not a regular file, on the opened
    /// descriptor BEFORE any read, write or truncate: a FIFO or directory
    /// substituted for the leaf fails immediately instead of blocking.
    /// Called inside `open_leaf_at` on every open, so every leaf in this
    /// store goes through the same nonblocking open + fstat regular-file
    /// check.
    fn require_regular(fd: BorrowedFd<'_>) -> Result<()> {
        let stat = rustix::fs::fstat(fd)?;
        let kind = rustix::fs::FileType::from_raw_mode(stat.st_mode);
        ensure!(
            kind == rustix::fs::FileType::RegularFile,
            "leaf is not a regular file"
        );
        Ok(())
    }
    /// Re-check a leaf descriptor is a regular file and clear `O_NONBLOCK`
    /// on the fd, returning a blocking `File`. The nonblocking open +
    /// fstat regular-file check happens on the fd at open time in
    /// `open_leaf_at`; the flag is cleared afterwards because regular files
    /// never block and the rest of this store relies on blocking reads
    /// rather than carrying the flag through std io.
    fn require_regular_and_block(fd: OwnedFd) -> Result<File> {
        Self::require_regular(fd.as_fd())?;
        let file = File::from(fd);
        fcntl_setfl(&file, fcntl_getfl(&file)? - OFlags::NONBLOCK)?;
        Ok(file)
    }
    /// One checked leaf helper for every leaf open: validates `name` with
    /// `check_name` FIRST, then opens with `flags` | O_NOFOLLOW|O_NONBLOCK|
    /// O_CLOEXEC and rejects non-regular files via fstat on the opened
    /// descriptor before it is returned, so a FIFO or directory substituted
    /// for the leaf fails immediately instead of blocking. `Ok(None)` means
    /// the leaf does not exist (NOENT); `Ok(Some(fd))` is a validated
    /// regular-file descriptor still carrying `O_NONBLOCK` — clear it with
    /// `require_regular_and_block` when blocking io is needed;
    /// `Err(StoreError::BadName)` means validation failed before any openat
    /// was issued.
    fn open_leaf_at(
        dir: BorrowedFd<'_>,
        name: &str,
        flags: OFlags,
        mode: Mode,
    ) -> Result<Option<OwnedFd>, StoreError> {
        Self::check_name(name).map_err(|_| StoreError::BadName)?;
        match openat(
            dir,
            name,
            flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            mode,
        ) {
            Ok(fd) => {
                // The regular-file check happens on the opened descriptor,
                // before the fd is returned: a FIFO or directory substituted
                // for the leaf is rejected here, never treated as present.
                Self::require_regular(fd.as_fd()).map_err(|e| StoreError::Open(e.to_string()))?;
                Ok(Some(fd))
            }
            Err(Errno::NOENT) => Ok(None),
            Err(code) => Err(StoreError::Open(format!("cannot open {name}: {code}"))),
        }
    }
    /// Write `value` as JSON to `leaf` atomically: fresh temp leaf in the same
    /// directory, full write, sync, rename over the leaf, directory sync.
    /// A symlinked `leaf` is replaced, never followed.
    fn write_leaf_atomic(
        dir: BorrowedFd<'_>,
        leaf: &str,
        value: &impl serde::Serialize,
    ) -> Result<()> {
        Self::check_name(leaf)?;
        let temp_name = format!("{leaf}.tmp-{}", uuid::Uuid::new_v4());
        let Some(temp_fd) = Self::open_leaf_at(
            dir,
            &temp_name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL,
            Mode::RUSR | Mode::WUSR,
        )?
        else {
            return Err(anyhow::anyhow!("leaf not created: {leaf}"));
        };
        let mut temp = Self::require_regular_and_block(temp_fd)?;
        temp.write_all(&serde_json::to_vec(value)?)?;
        temp.sync_all()?;
        renameat(dir, temp_name, dir, leaf)?;
        fsync(dir)?;
        Ok(())
    }
    fn job_fd(&self, id: &str) -> Result<OwnedFd> {
        valid_id(id)?;
        Self::open_dir_at(self.root_fd.as_fd(), id)
    }
    fn control_fd(&self, id: &str) -> Result<OwnedFd> {
        Self::open_dir_at(self.job_fd(id)?.as_fd(), CONTROL_DIR)
    }
    /// True only when the job directory exists as a real directory. A
    /// symlink, whether dangling or not, and a non-directory entry are `Err`.
    pub fn job_exists(&self, id: &str) -> Result<bool> {
        valid_id(id)?;
        Self::check_name(id)?;
        match openat(
            self.root_fd.as_fd(),
            id,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => {
                // OwnedFd closes the descriptor on drop.
                drop(fd);
                Ok(true)
            }
            Err(Errno::NOENT) => Ok(false),
            Err(code) => Err(anyhow::anyhow!("cannot open directory {id}: {code}")),
        }
    }
    /// Create the per-request job directory and its control directory.
    pub fn create_job(&self, id: &str) -> Result<()> {
        valid_id(id)?;
        mkdirat(self.root_fd.as_fd(), id, Mode::RWXU)?;
        let job = self.job_fd(id)?;
        mkdirat(job.as_fd(), CONTROL_DIR, Mode::RWXU)?;
        fsync(self.root_fd.as_fd())?;
        fsync(job.as_fd())?;
        Ok(())
    }
    /// Plain checkout directory for a job; it must exist as a real directory.
    pub fn dir(&self, id: &str) -> Result<PathBuf> {
        self.job_fd(id)?;
        Ok(self.root.join(id))
    }
    /// Create and lock the `.lock` leaf at the job root.
    pub fn lock(&self) -> Result<File> {
        let Some(fd) = Self::open_leaf_at(
            self.root_fd.as_fd(),
            LOCK_LEAF,
            OFlags::RDWR | OFlags::CREATE,
            Mode::RUSR | Mode::WUSR,
        )?
        else {
            return Err(anyhow::anyhow!("lock leaf not created"));
        };
        let file = Self::require_regular_and_block(fd)?;
        file.lock()?;
        Ok(file)
    }
    /// Read the job record and confirm its self-consistent ownership.
    pub fn read(&self, id: &str) -> Result<JobRecord> {
        let Some(fd) = Self::open_leaf_at(
            self.control_fd(id)?.as_fd(),
            RECORD_LEAF,
            OFlags::RDONLY,
            Mode::empty(),
        )?
        else {
            return Err(anyhow::anyhow!("record leaf absent for {id}"));
        };
        let mut file = Self::require_regular_and_block(fd)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let record: JobRecord = serde_json::from_slice(&bytes)?;
        ensure!(
            record.request.request_id == id
                && record.request.fingerprint() == record.snapshot.request_sha256,
            "job ownership mismatch"
        );
        Ok(record)
    }
    /// Atomically write the record and bump its revision.
    pub fn write(&self, record: &mut JobRecord) -> Result<()> {
        record.snapshot.revision = record.snapshot.revision.saturating_add(1);
        Self::write_leaf_atomic(
            self.control_fd(&record.request.request_id)?.as_fd(),
            RECORD_LEAF,
            record,
        )
    }
    /// Create the worker log leaf fresh; a pre-existing leaf is an error.
    pub fn create_log(&self, id: &str) -> Result<File> {
        let Some(fd) = Self::open_leaf_at(
            self.control_fd(id)?.as_fd(),
            LOG_LEAF,
            OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE | OFlags::EXCL,
            Mode::RUSR | Mode::WUSR,
        )?
        else {
            return Err(anyhow::anyhow!("log leaf not created for {id}"));
        };
        Self::require_regular_and_block(fd)
    }
    /// Append to the worker log leaf, creating it if needed.
    pub fn log_for_append(&self, id: &str) -> Result<File> {
        let Some(fd) = Self::open_leaf_at(
            self.control_fd(id)?.as_fd(),
            LOG_LEAF,
            OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE,
            Mode::RUSR | Mode::WUSR,
        )?
        else {
            return Err(anyhow::anyhow!("log leaf not created for {id}"));
        };
        Self::require_regular_and_block(fd)
    }
    /// Log page reader: a missing log leaf yields an empty page; a symlinked
    /// or non-regular log leaf (including a FIFO) is rejected through the
    /// nonblocking regular-leaf helper. The leaf name is a crate constant
    /// checked through the same `check_name` path the caller-supplied
    /// helpers use, so every openat in this file is preceded by a name
    /// check.
    pub fn logs(&self, query: LogsQuery) -> Result<LogsPage> {
        self.read(&query.request_id)?;
        let control = self.control_fd(&query.request_id)?;
        // One checked leaf helper: NOENT means the log is absent, so a missing
        // log is an empty page, not an error; a FIFO or directory leaf errors.
        let Some(log_fd) =
            Self::open_leaf_at(control.as_fd(), LOG_LEAF, OFlags::RDONLY, Mode::empty())?
        else {
            return Ok(LogsPage {
                cursor: 0,
                next_cursor: 0,
                bytes: vec![],
            });
        };
        let mut file = Self::require_regular_and_block(log_fd)?;
        let len = file.metadata()?.len();
        let cursor = query.cursor.min(len);
        file.seek(SeekFrom::Start(cursor))?;
        let mut bytes = vec![];
        file.take(query.max_bytes.min(65536) as u64)
            .read_to_end(&mut bytes)?;
        Ok(LogsPage {
            cursor,
            next_cursor: cursor + bytes.len() as u64,
            bytes,
        })
    }
    /// Write the provenance leaf into the job directory, create-new only.
    pub fn write_spec(&self, id: &str, text: &str) -> Result<()> {
        let Some(fd) = Self::open_leaf_at(
            self.job_fd(id)?.as_fd(),
            SPEC_LEAF,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
            Mode::RUSR | Mode::WUSR,
        )?
        else {
            return Err(anyhow::anyhow!("spec leaf not created for {id}"));
        };
        let mut file = Self::require_regular_and_block(fd)?;
        file.write_all(text.as_bytes())?;
        Ok(())
    }
    /// Open or create the branch reservation directory at the job root.
    fn branches_fd(&self) -> Result<OwnedFd> {
        match mkdirat(self.root_fd.as_fd(), BRANCHES_DIR, Mode::RWXU) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(code) => return Err(anyhow::anyhow!("cannot open {BRANCHES_DIR}: {code}")),
        }
        Self::open_dir_at(self.root_fd.as_fd(), BRANCHES_DIR)
    }
    /// True only when the branch reservation leaf exists as a regular file,
    /// through the nonblocking regular-leaf helper: a directory or FIFO at
    /// the reservation leaf is rejected, never treated as reserved. A missing
    /// leaf still means `Ok(false)`. The caller-supplied `key` is the FIRST
    /// thing checked: `check_name` runs before `branches_fd()` or anything
    /// else that can create or open `branches/`, so multi-component, dotted,
    /// absolute, empty or NUL-containing keys error out before a single openat
    /// or mkdirat is issued.
    pub fn branch_reserved(&self, key: &str) -> Result<bool> {
        Self::check_name(key).map_err(|_| StoreError::BadName)?;
        let branches = self.branches_fd()?;
        match Self::open_leaf_at(branches.as_fd(), key, OFlags::RDONLY, Mode::empty()) {
            Ok(Some(fd)) => {
                // The helper fstat-checked the leaf before it was returned;
                // a directory or FIFO is an error, never "reserved".
                drop(Self::require_regular_and_block(fd)?);
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(e) => Err(anyhow::Error::from(e)),
        }
    }
    /// Atomically record which request owns a branch.
    pub fn reserve_branch(&self, key: &str, id: &str) -> Result<()> {
        Self::check_name(key)?;
        valid_id(id)?;
        let branches = self.branches_fd()?;
        Self::write_leaf_atomic(branches.as_fd(), key, &id)
    }
    /// The request that holds a branch reservation, or `None` when the branch
    /// is free. A reservation leaf that is not a regular file or does not
    /// hold a request ID is an error, never "free".
    pub fn branch_owner(&self, key: &str) -> Result<Option<String>> {
        Self::check_name(key).map_err(|_| StoreError::BadName)?;
        let branches = self.branches_fd()?;
        let Some(fd) = Self::open_leaf_at(branches.as_fd(), key, OFlags::RDONLY, Mode::empty())?
        else {
            return Ok(None);
        };
        let mut file = Self::require_regular_and_block(fd)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let owner: String = serde_json::from_slice(&bytes)?;
        valid_id(&owner)?;
        Ok(Some(owner))
    }
    /// Drop a branch reservation, but only while `id` still holds it.
    pub fn release_branch(&self, key: &str, id: &str) -> Result<()> {
        if self.branch_owner(key)?.as_deref() == Some(id) {
            let branches = self.branches_fd()?;
            rustix::fs::unlinkat(branches.as_fd(), key, rustix::fs::AtFlags::empty())?;
            fsync(branches.as_fd())?;
        }
        Ok(())
    }
    /// True when the job's record leaf exists (a regular file, no-follow). A
    /// job directory whose control directory was never created has no record.
    pub fn record_exists(&self, id: &str) -> Result<bool> {
        let job = self.job_fd(id)?;
        let control = match openat(
            job.as_fd(),
            CONTROL_DIR,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(false),
            Err(code) => {
                return Err(anyhow::anyhow!(
                    "cannot open directory {CONTROL_DIR}: {code}"
                ))
            }
        };
        Ok(
            Self::open_leaf_at(control.as_fd(), RECORD_LEAF, OFlags::RDONLY, Mode::empty())?
                .is_some(),
        )
    }
    /// Remove a job directory that a submission created but never finished:
    /// it may hold only the empty control directory. Anything else in it
    /// (a record, a log, a checkout) makes this fail, so a job that could
    /// have started is never removed.
    pub fn remove_unstarted_job(&self, id: &str) -> Result<()> {
        let job = self.job_fd(id)?;
        let mut entries = rustix::fs::Dir::read_from(job.as_fd())?;
        while let Some(entry) = entries.read() {
            let name = entry?.file_name().to_bytes().to_vec();
            ensure!(
                matches!(name.as_slice(), b"." | b"..") || name == CONTROL_DIR.as_bytes(),
                "job {id} is not unstarted"
            );
        }
        match rustix::fs::unlinkat(job.as_fd(), CONTROL_DIR, rustix::fs::AtFlags::REMOVEDIR) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(code) => return Err(anyhow::anyhow!("job {id} is not unstarted: {code}")),
        }
        drop(job);
        rustix::fs::unlinkat(self.root_fd.as_fd(), id, rustix::fs::AtFlags::REMOVEDIR)
            .map_err(|code| anyhow::anyhow!("job {id} is not unstarted: {code}"))?;
        fsync(self.root_fd.as_fd())?;
        Ok(())
    }
}
