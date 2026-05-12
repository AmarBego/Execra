//! Process-group cancellation. The runtime must kill descendants too — when
//! `scoop install` is cancelled, the `aria2` it spawned has to die with it.
//!
//! - Windows: assign the child to a Win32 Job Object with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. `TerminateJobObject` kills the
//!   whole tree; dropping the handle also kills anything still attached.
//! - Unix: spawn the child with its own process group (`setpgid` via
//!   `Command::process_group(0)`); `SIGTERM` starts graceful cancellation and
//!   `SIGKILL` force-kills the tree after the configured grace period.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct ProcessGroup {
        job: HANDLE,
    }

    impl ProcessGroup {
        pub fn configure(_cmd: &mut tokio::process::Command) {
            // No-op on Windows; the Job Object is created post-spawn and the
            // child inherits the assignment automatically.
        }

        pub fn assign(child: &tokio::process::Child) -> Option<Self> {
            // SAFETY: All Win32 calls below check return codes and we own the
            // resulting handle until Drop. Handles are valid across threads.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }

                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    CloseHandle(job);
                    return None;
                }

                let proc_handle: HANDLE = match child.raw_handle() {
                    Some(h) => h as HANDLE,
                    None => {
                        CloseHandle(job);
                        return None;
                    }
                };

                let ok = AssignProcessToJobObject(job, proc_handle);
                if ok == 0 {
                    CloseHandle(job);
                    return None;
                }

                Some(ProcessGroup { job })
            }
        }

        /// Best-effort graceful termination. Job Objects only support
        /// forceful termination, so graceful and forceful are equivalent.
        pub fn terminate(&self) {
            self.kill();
        }

        /// Synchronously terminates every process in the group.
        pub fn kill(&self) {
            unsafe {
                TerminateJobObject(self.job, 1);
            }
        }
    }

    impl Drop for ProcessGroup {
        fn drop(&mut self) {
            // Closing the last handle to the job triggers KILL_ON_JOB_CLOSE,
            // which is what we want as a final safety net.
            unsafe {
                CloseHandle(self.job);
            }
        }
    }

    // SAFETY: HANDLE is an opaque kernel reference; concurrent access is fine
    // as long as we don't double-close (we hold it until Drop).
    unsafe impl Send for ProcessGroup {}
    unsafe impl Sync for ProcessGroup {}
}

#[cfg(unix)]
mod imp {
    pub struct ProcessGroup {
        pgid: i32,
    }

    impl ProcessGroup {
        pub fn configure(cmd: &mut tokio::process::Command) {
            // Put the child in its own process group, with pgid == child pid.
            cmd.process_group(0);
        }

        pub fn assign(child: &tokio::process::Child) -> Option<Self> {
            let pid = child.id()? as i32;
            Some(ProcessGroup { pgid: pid })
        }

        pub fn terminate(&self) {
            unsafe {
                libc::killpg(self.pgid, libc::SIGTERM);
            }
        }

        pub fn kill(&self) {
            unsafe {
                libc::killpg(self.pgid, libc::SIGKILL);
            }
        }
    }
}

pub use imp::ProcessGroup;
