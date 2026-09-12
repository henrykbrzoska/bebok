//! Windows-only process-tree management via a Job Object.
//!
//! ConPTY children are spawned by `portable-pty`; to guarantee that killing a
//! terminal session also kills the whole tree (shell + descendants), we assign
//! the child to a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and
//! terminate the job on kill. When that fails, the caller falls back to
//! `taskkill /T /F /PID`.

use std::os::windows::io::RawHandle;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};

/// A Job Object handle that is safe to share across threads. `HANDLE` is a raw
/// pointer, but a kernel object handle is opaque and has no thread affinity, so
/// it is sound to mark it `Send + Sync`.
pub struct JobObject(pub HANDLE);

// SAFETY: `HANDLE` here is an opaque kernel handle, not a pointer to memory.
unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}

/// Create a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and assign
/// `process_handle` to it. Returns the job handle, or `None` on failure (the
/// caller keeps the `taskkill /T` fallback).
pub fn assign_process(process_handle: RawHandle) -> Option<JobObject> {
    unsafe {
        let job = CreateJobObjectW(None, windows::core::PCWSTR::null()).ok()?;

        // Kill every process in the job when the last handle to it is closed,
        // and terminate the whole tree on `terminate()`.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            size,
        )
        .is_err()
        {
            let _ = CloseHandle(job);
            return None;
        }

        if AssignProcessToJobObject(job, HANDLE(process_handle)).is_err() {
            let _ = CloseHandle(job);
            return None;
        }

        Some(JobObject(job))
    }
}

/// Terminate every process in the job and close the handle.
pub fn terminate(job: JobObject) {
    unsafe {
        let _ = TerminateJobObject(job.0, 1);
        let _ = CloseHandle(job.0);
    }
}
