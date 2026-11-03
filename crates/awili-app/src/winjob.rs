//! Windows Job Objects: one per spawned vendor CLI or MCP server, so Stop kills
//! the whole process tree (the process, its tools, and anything they started,
//! even grandchildren whose parent already exited, which `taskkill /T` misses).
//!
//! Every process is put in its job ATOMICALLY and BY ITS OWN HANDLE: it is
//! created suspended, the handle `CreateProcess` returned is assigned to a job
//! with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and only then is it resumed, so
//! nothing it launches can escape and no pid is ever looked up (no pid-reuse
//! race). The job is kept in a registry keyed by pid; [`terminate`] ends a
//! registered job and nothing else. A pid Awili did not start has no entry and
//! is never touched. If Awili itself exits, the handles close and the kernel
//! ends every job's processes.

use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::pin::Pin;
use std::process::{Child, Command, ExitStatus};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_SUSPENDED: u32 = 0x0000_0004;

#[link(name = "ntdll")]
extern "system" {
    fn NtResumeProcess(process: HANDLE) -> i32;
}

/// A job handle. Raw handles are plain integers, safe to move between threads.
struct Job(isize);

impl Job {
    fn handle(&self) -> HANDLE {
        self.0 as HANDLE
    }

    fn active_processes(&self) -> Option<u32> {
        // SAFETY: an all-zero accounting struct is a valid out buffer.
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: info is a valid out buffer of the stated size.
        let ok = unsafe {
            QueryInformationJobObject(
                self.handle(),
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut c_void,
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(info.ActiveProcesses)
    }

    fn terminate(&self) -> io::Result<()> {
        // SAFETY: the job handle is valid while `self` lives.
        if unsafe { TerminateJobObject(self.handle(), 1) } == 0 {
            return Err(last_error("TerminateJobObject"));
        }
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Closing the last handle also ends any process still in the job
        // (KILL_ON_JOB_CLOSE).
        // SAFETY: the handle was created by CreateJobObjectW and is owned here.
        unsafe { CloseHandle(self.handle()) };
    }
}

static JOBS: Mutex<Option<HashMap<u32, Arc<Job>>>> = Mutex::new(None);

fn last_error(what: &str) -> io::Error {
    // SAFETY: no preconditions.
    let code = unsafe { GetLastError() };
    io::Error::other(format!("{what} failed (Windows error {code})"))
}

fn new_job() -> io::Result<Job> {
    // SAFETY: null attributes and name: an anonymous job with default security.
    let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if h.is_null() {
        return Err(last_error("CreateJobObjectW"));
    }
    let job = Job(h as isize);
    // SAFETY: an all-zero limit struct is valid; only the flag is set.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: info is a valid, initialised struct of the stated size.
    let ok = unsafe {
        SetInformationJobObject(
            job.handle(),
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        return Err(last_error("SetInformationJobObject"));
    }
    Ok(job)
}

fn register(pid: u32, job: Arc<Job>) {
    let mut guard = JOBS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    // Forget jobs whose processes are all gone (closing the handle of an empty
    // job is harmless), so the registry does not grow with every run.
    map.retain(|_, j| j.active_processes().unwrap_or(1) > 0);
    map.insert(pid, job);
}

/// Put the (suspended) process with this handle in a new job, resume it, and
/// register the job under `pid`. On failure the caller must end the process.
fn confine_and_resume(pid: u32, process: HANDLE) -> io::Result<Arc<Job>> {
    let job = Arc::new(new_job()?);
    // SAFETY: both handles are valid; the process is suspended.
    if unsafe { AssignProcessToJobObject(job.handle(), process) } == 0 {
        return Err(last_error("AssignProcessToJobObject"));
    }
    // SAFETY: the process handle is valid and the process is suspended.
    let status = unsafe { NtResumeProcess(process) };
    if status < 0 {
        return Err(io::Error::other(format!(
            "could not start the process (NtResumeProcess {status:#x})"
        )));
    }
    register(pid, job.clone());
    Ok(job)
}

/// Spawn `cmd` inside a new Job Object. Its creation flags are set here to
/// new-process-group + no console window + suspended. If the job cannot be set
/// up the child is killed and the error returned: a run that could not be made
/// stoppable does not start.
pub(crate) fn spawn_in_job(cmd: &mut Command) -> io::Result<Child> {
    // Never let the OS search for a program by bare name: that search can reach
    // the project folder or a relative `PATH` entry. Resolution already found an
    // absolute path (`work::resolve_agent_bin`); a bare name means "not found".
    if !std::path::Path::new(cmd.get_program()).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} was not found", cmd.get_program().to_string_lossy()),
        ));
    }
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_SUSPENDED);
    let mut child = cmd.spawn()?;
    // The handle std got from CreateProcess: the process itself, no pid lookup.
    match confine_and_resume(child.id(), child.as_raw_handle() as HANDLE) {
        Ok(_) => Ok(child),
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(io::Error::other(format!(
                "could not start this process so Stop can end all of it: {e}"
            )))
        }
    }
}

/// End every process in `pid`'s job. `Ok(true)`: terminated. `Ok(false)`: Awili
/// holds no job for this pid (it did not start it, or it is long gone), and
/// nothing was touched. `Err`: terminating failed (the job stays registered).
pub(crate) fn terminate(pid: u32) -> io::Result<bool> {
    let job = {
        let guard = JOBS.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().and_then(|m| m.get(&pid).cloned())
    };
    let Some(job) = job else { return Ok(false) };
    job.terminate()?;
    let mut guard = JOBS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = guard.as_mut() {
        if m.get(&pid).is_some_and(|j| Arc::ptr_eq(j, &job)) {
            m.remove(&pid);
        }
    }
    Ok(true)
}

/// Processes still alive in `pid`'s job (tests and diagnostics).
#[cfg(test)]
pub(crate) fn active_in_job(pid: u32) -> Option<u32> {
    let guard = JOBS.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref()?.get(&pid)?.active_processes()
}

// ---- MCP servers: spawned inside rmcp, confined through a process-wrap hook ----

use process_wrap::tokio::{ChildWrapper, CommandWrap, CommandWrapper};

/// A `process-wrap` wrapper that gives a child spawned by a library the same
/// atomic, handle-based confinement as [`spawn_in_job`]: created suspended,
/// assigned by the child's own handle, resumed, registered. Dropping the child
/// ends its tree.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TrackedJob;

impl CommandWrapper for TrackedJob {
    fn pre_spawn(
        &mut self,
        command: &mut tokio::process::Command,
        _core: &CommandWrap,
    ) -> io::Result<()> {
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_SUSPENDED);
        Ok(())
    }

    fn wrap_child(
        &mut self,
        mut inner: Box<dyn ChildWrapper>,
        _core: &CommandWrap,
    ) -> io::Result<Box<dyn ChildWrapper>> {
        let pid = inner
            .id()
            .ok_or_else(|| io::Error::other("the child has no process id"))?;
        let handle = inner
            .inner_child()
            .raw_handle()
            .ok_or_else(|| io::Error::other("the child has no process handle"))?;
        match confine_and_resume(pid, handle as HANDLE) {
            Ok(job) => Ok(Box::new(TrackedChild {
                inner: Some(inner),
                job,
                pid,
            })),
            Err(e) => {
                let _ = inner.start_kill();
                Err(io::Error::other(format!(
                    "could not start this process so Stop can end all of it: {e}"
                )))
            }
        }
    }
}

/// The child half of [`TrackedJob`]: kills the whole job, and on drop forgets
/// the registry entry so the job handle closes and ends any leftovers.
#[derive(Debug)]
struct TrackedChild {
    /// `None` only after `into_inner` took it.
    inner: Option<Box<dyn ChildWrapper>>,
    job: Arc<Job>,
    pid: u32,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Job({:#x})", self.0)
    }
}

impl TrackedChild {
    fn child(&self) -> &dyn ChildWrapper {
        self.inner
            .as_deref()
            .expect("child present until into_inner")
    }
    fn child_mut(&mut self) -> &mut dyn ChildWrapper {
        self.inner
            .as_deref_mut()
            .expect("child present until into_inner")
    }
}

impl ChildWrapper for TrackedChild {
    fn inner(&self) -> &dyn ChildWrapper {
        self.child().inner()
    }
    fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
        self.child_mut().inner_mut()
    }
    fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
        // The job stays registered (Drop skips it once `inner` is taken): the
        // caller keeps using the child.
        self.inner.take().expect("child present until into_inner")
    }
    fn start_kill(&mut self) -> io::Result<()> {
        self.job.terminate()
    }
    fn wait(
        &mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = io::Result<ExitStatus>> + Send + '_>> {
        self.child_mut().wait()
    }
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child_mut().try_wait()
    }
}

impl Drop for TrackedChild {
    fn drop(&mut self) {
        if self.inner.is_none() {
            return; // handed on by into_inner: the registry keeps the job
        }
        let mut guard = JOBS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = guard.as_mut() {
            if m.get(&self.pid).is_some_and(|j| Arc::ptr_eq(j, &self.job)) {
                m.remove(&self.pid);
            }
        }
        // `self.job` drops after this: the last reference closes the handle,
        // which ends whatever is still in the job.
    }
}
