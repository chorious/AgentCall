use serde::Serialize;
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) enum ProcessControllerKind {
    PortablePtyBestEffort,
    #[cfg(windows)]
    WindowsJob,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProcessHandle {
    pub(crate) session_id: String,
    pub(crate) child_pid: Option<u32>,
    pub(crate) controller: ProcessControllerKind,
    #[serde(skip)]
    job: Option<Arc<WindowsJobHandle>>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct KillResult {
    pub(crate) requested: bool,
    pub(crate) child_pid: Option<u32>,
    pub(crate) controller: ProcessControllerKind,
    pub(crate) cleanup_guarantee: &'static str,
    pub(crate) fallback_used: bool,
    pub(crate) error: Option<String>,
}

impl ProcessHandle {
    pub(crate) fn create(session_id: &str, child_pid: Option<u32>) -> Self {
        #[cfg(windows)]
        if let Some(pid) = child_pid {
            if let Ok(job) = WindowsJobHandle::assign(pid) {
                return Self {
                    session_id: session_id.to_string(),
                    child_pid,
                    controller: ProcessControllerKind::WindowsJob,
                    job: Some(Arc::new(job)),
                };
            }
        }
        Self {
            session_id: session_id.to_string(),
            child_pid,
            controller: ProcessControllerKind::PortablePtyBestEffort,
            job: None,
        }
    }

    pub(crate) fn kill_tree(&self) -> KillResult {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            match job.terminate(1) {
                Ok(()) => {
                    return KillResult {
                        requested: true,
                        child_pid: self.child_pid,
                        controller: ProcessControllerKind::WindowsJob,
                        cleanup_guarantee: "windows_job_terminate",
                        fallback_used: false,
                        error: None,
                    };
                }
                Err(err) => {
                    return KillResult {
                        requested: false,
                        child_pid: self.child_pid,
                        controller: ProcessControllerKind::WindowsJob,
                        cleanup_guarantee: "windows_job_terminate_failed",
                        fallback_used: true,
                        error: Some(err),
                    };
                }
            }
        }
        KillResult {
            requested: true,
            child_pid: self.child_pid,
            controller: ProcessControllerKind::PortablePtyBestEffort,
            cleanup_guarantee: "best_effort_parent_kill_only",
            fallback_used: true,
            error: None,
        }
    }
}

pub(crate) fn default_process_controller_kind() -> ProcessControllerKind {
    #[cfg(windows)]
    {
        ProcessControllerKind::WindowsJob
    }
    #[cfg(not(windows))]
    {
        ProcessControllerKind::PortablePtyBestEffort
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct WindowsJobHandle {
    handle: isize,
}

#[cfg(not(windows))]
#[derive(Debug)]
struct WindowsJobHandle;

#[cfg(windows)]
unsafe impl Send for WindowsJobHandle {}
#[cfg(windows)]
unsafe impl Sync for WindowsJobHandle {}

#[cfg(windows)]
impl WindowsJobHandle {
    fn assign(pid: u32) -> Result<Self, String> {
        use std::mem::size_of;
        use std::ptr::null;
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        };

        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return Err(format!("CreateJobObjectW failed: {}", GetLastError()));
            }
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set_ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set_ok == 0 {
                let err = GetLastError();
                CloseHandle(job);
                return Err(format!("SetInformationJobObject failed: {err}"));
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                let err = GetLastError();
                CloseHandle(job);
                return Err(format!("OpenProcess failed for pid {pid}: {err}"));
            }
            let assign_ok = AssignProcessToJobObject(job, process);
            let assign_error = GetLastError();
            CloseHandle(process);
            if assign_ok == 0 {
                CloseHandle(job);
                return Err(format!(
                    "AssignProcessToJobObject failed for pid {pid}: {assign_error}"
                ));
            }
            Ok(Self {
                handle: job as isize,
            })
        }
    }

    fn terminate(&self, exit_code: u32) -> Result<(), String> {
        use windows_sys::Win32::Foundation::GetLastError;
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        unsafe {
            if TerminateJobObject(self.handle as _, exit_code) == 0 {
                return Err(format!("TerminateJobObject failed: {}", GetLastError()));
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for WindowsJobHandle {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            if self.handle != 0 {
                CloseHandle(self.handle as _);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_handle_falls_back_without_pid() {
        let handle = ProcessHandle::create("worker-a", None);
        assert_eq!(handle.child_pid, None);
        assert_eq!(
            handle.controller,
            ProcessControllerKind::PortablePtyBestEffort
        );
        let result = handle.kill_tree();
        assert!(result.requested);
        assert!(result.fallback_used);
        assert_eq!(result.cleanup_guarantee, "best_effort_parent_kill_only");
    }
}
