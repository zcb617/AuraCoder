#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 绑定到当前进程的 Job Object，句柄关闭时由内核结束全部登记子进程。
///
/// 该 Job 设置了 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`：当本进程退出
/// （正常退出、崩溃或被 `TerminateProcess` 强杀）导致最后一个 Job 句柄被内核
/// 自动关闭时，内核会一并终止 Job 中登记的所有子进程。这是 Windows 官方提供的
/// “父关子闭”机制，用于弥补 tokio `kill_on_drop` 在进程被强杀时无法运行 Drop
/// 的缺陷。句柄只由本进程持有，保证父进程死亡即触发。
///
/// 依据 Microsoft Learn:
/// - Job Objects: 关闭最后一个 Job 句柄且设置该标志时终止全部关联进程。
/// - Terminating a Process: 进程终止时其持有的内核句柄被自动关闭。
#[cfg(target_os = "windows")]
struct KillOnCloseJob {
    handle: windows::Win32::Foundation::HANDLE,
}

// Job 句柄是内核对象句柄，可被多个线程安全地用于 AssignProcessToJobObject；
// windows crate 的 HANDLE 含裸指针未实现 Send/Sync，这里手动保证线程安全。
#[cfg(target_os = "windows")]
unsafe impl Send for KillOnCloseJob {}
#[cfg(target_os = "windows")]
unsafe impl Sync for KillOnCloseJob {}

#[cfg(target_os = "windows")]
impl KillOnCloseJob {
    fn create() -> std::io::Result<Self> {
        use windows::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        unsafe {
            let handle = CreateJobObjectW(None, None)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
                BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                    LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                    ..Default::default()
                },
                ..Default::default()
            };
            let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as *const _,
                size,
            )
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?;
            // 只挂显式登记的子进程，不把当前进程挂进 Job：
            // 挂当前进程会让本进程后续 CreateProcess 的所有后代默认进入该 Job，
            // 误伤与 OpenCode 无关的子进程。
            Ok(Self { handle })
        }
    }

    /// 将一个已启动的子进程登记进 Job；登记成功后该子进程随本进程退出被内核结束。
    fn assign_child(&self, child: &tokio::process::Child) -> std::io::Result<()> {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::JobObjects::AssignProcessToJobObject;

        let raw = child
            .raw_handle()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "子进程句柄不可用"))?;
        unsafe {
            AssignProcessToJobObject(self.handle, HANDLE(raw as _))
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))
        }
    }
}

// Job 句柄由 Drop/CloseHandle 释放；内核保证进程退出时即使未 Drop 也会关闭。
#[cfg(target_os = "windows")]
impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(target_os = "windows")]
impl std::fmt::Debug for KillOnCloseJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KillOnCloseJob").finish_non_exhaustive()
    }
}

#[cfg(target_os = "windows")]
fn kill_on_close_job() -> Option<&'static KillOnCloseJob> {
    use std::sync::OnceLock;

    static JOB: OnceLock<Option<KillOnCloseJob>> = OnceLock::new();
    JOB.get_or_init(|| match KillOnCloseJob::create() {
        Ok(job) => Some(job),
        Err(error) => {
            log::warn!("创建 Windows 子进程收尸 Job 失败，回退到 kill_on_drop 兜底: {error}");
            None
        }
    })
    .as_ref()
}

/// 将 tokio 子进程登记进“父死子亡”Job；仅 Windows 生效，其余平台为空操作。
///
/// 该函数只负责内核级兜底登记，不取代已有的 `kill_on_drop` 与显式 `terminate`；
/// 失败时记录告警并返回错误，由调用方决定是否继续（登记失败不影响子进程已启动）。
#[cfg(target_os = "windows")]
pub fn register_child_kill_on_close(child: &tokio::process::Child) -> std::io::Result<()> {
    match kill_on_close_job() {
        Some(job) => job.assign_child(child),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Windows 子进程收尸 Job 不可用",
        )),
    }
}

#[cfg(not(target_os = "windows"))]
pub fn register_child_kill_on_close(_child: &tokio::process::Child) -> std::io::Result<()> {
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn configure_std_command(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
pub fn configure_std_command(_command: &mut std::process::Command) {}

#[cfg(target_os = "windows")]
pub fn configure_tokio_command(command: &mut tokio::process::Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
pub fn configure_tokio_command(_command: &mut tokio::process::Command) {}

/// Serializes tests that mutate process-global environment variables (PATH,
/// HOME, ...) against tests that spawn subprocesses. env mutation is
/// process-wide, so a parallel test spawning `git` while another test points
/// PATH at an empty temp dir fails with ENOENT.
#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}
