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
        #[cfg(windows)]
        if let Some(pid) = self.child_pid {
            let output = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .output();
            return match output {
                Ok(output) if output.status.success() => KillResult {
                    requested: true,
                    child_pid: self.child_pid,
                    controller: ProcessControllerKind::PortablePtyBestEffort,
                    cleanup_guarantee: "windows_taskkill_tree_fallback",
                    fallback_used: true,
                    error: None,
                },
                Ok(output) => KillResult {
                    requested: false,
                    child_pid: self.child_pid,
                    controller: ProcessControllerKind::PortablePtyBestEffort,
                    cleanup_guarantee: "windows_taskkill_tree_failed",
                    fallback_used: true,
                    error: Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
                },
                Err(err) => KillResult {
                    requested: false,
                    child_pid: self.child_pid,
                    controller: ProcessControllerKind::PortablePtyBestEffort,
                    cleanup_guarantee: "windows_taskkill_tree_spawn_failed",
                    fallback_used: true,
                    error: Some(err.to_string()),
                },
            };
        }
        KillResult {
            requested: self.child_pid.is_some(),
            child_pid: self.child_pid,
            controller: ProcessControllerKind::PortablePtyBestEffort,
            cleanup_guarantee: "best_effort_no_process_controller",
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
// SAFETY: WindowsJobHandle owns an OS handle value and only exposes operations
// that call Win32 APIs with that handle. The handle is closed exactly once in
// Drop, and no borrowed Rust data is shared through it.
unsafe impl Send for WindowsJobHandle {}
#[cfg(windows)]
// SAFETY: The underlying Win32 job handle can be used from multiple threads by
// the OS. Methods take &self and do not mutate Rust-owned memory.
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
        assert!(!result.requested);
        assert!(result.fallback_used);
        assert_eq!(
            result.cleanup_guarantee,
            "best_effort_no_process_controller"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_job_kill_cleans_child_process_tree_when_assignable() {
        use std::fs;
        use std::process::Command;
        use std::thread;
        use std::time::{Duration, Instant};

        let root = std::env::temp_dir().join(format!(
            "agentcall-job-smoke-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).unwrap();
        let script_path = root.join("parent.ps1");
        let flag_path = root.join("start-child.flag");
        let child_pid_path = root.join("child.pid");
        fs::write(
            &script_path,
            r#"
param([string]$FlagPath, [string]$ChildPidPath)
while (-not (Test-Path -LiteralPath $FlagPath)) {
  Start-Sleep -Milliseconds 50
}
$child = Start-Process -FilePath cmd.exe -ArgumentList @('/C','ping -n 120 127.0.0.1 >NUL') -PassThru
Set-Content -LiteralPath $ChildPidPath -Value $child.Id -Encoding ascii
Start-Sleep -Seconds 120
"#,
        )
        .unwrap();

        let mut parent = Command::new("powershell.exe")
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&script_path)
            .arg("-FlagPath")
            .arg(&flag_path)
            .arg("-ChildPidPath")
            .arg(&child_pid_path)
            .spawn()
            .unwrap();
        let parent_pid = parent.id();
        let handle = ProcessHandle::create("job-smoke", Some(parent_pid));
        if handle.controller != ProcessControllerKind::WindowsJob {
            let _ = parent.kill();
            let _ = parent.wait();
            let _ = fs::remove_dir_all(root);
            return;
        }

        fs::write(&flag_path, "go").unwrap();
        let child_pid = wait_for_pid_file(&child_pid_path, Duration::from_secs(5));
        if !pid_is_running(child_pid) {
            eprintln!(
                "skipping windows job child-tree assertion: child pid {child_pid} exited before kill_tree could be exercised"
            );
            let _ = parent.kill();
            let _ = parent.wait();
            let _ = fs::remove_dir_all(root);
            return;
        }
        let parent_running_before_kill = pid_is_running(parent_pid);

        let result = handle.kill_tree();
        assert!(result.requested, "{result:?}");
        assert!(!result.fallback_used, "{result:?}");
        assert_eq!(result.cleanup_guarantee, "windows_job_terminate");

        let _ = parent.wait();
        if parent_running_before_kill {
            assert!(
                wait_until_not_running(parent_pid, Duration::from_secs(5)),
                "parent pid {parent_pid} still running"
            );
        }
        assert!(
            wait_until_not_running(child_pid, Duration::from_secs(5)),
            "child pid {child_pid} still running"
        );
        let _ = fs::remove_dir_all(root);

        fn wait_for_pid_file(path: &std::path::Path, timeout: Duration) -> u32 {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if let Ok(text) = fs::read_to_string(path) {
                    if let Ok(pid) = text.trim().parse::<u32>() {
                        return pid;
                    }
                }
                thread::sleep(Duration::from_millis(50));
            }
            panic!("timed out waiting for child pid file: {}", path.display());
        }

        fn wait_until_not_running(pid: u32, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if !pid_is_running(pid) {
                    return true;
                }
                thread::sleep(Duration::from_millis(50));
            }
            !pid_is_running(pid)
        }

        fn pid_is_running(pid: u32) -> bool {
            let output = Command::new("tasklist")
                .arg("/FI")
                .arg(format!("PID eq {pid}"))
                .output();
            let Ok(output) = output else {
                return false;
            };
            String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
        }
    }
}
