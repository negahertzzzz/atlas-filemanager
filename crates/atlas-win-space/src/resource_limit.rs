//! A user-chosen ceiling on how much of the machine Atlas may use.
//!
//! Atlas joins a job object whose hard CPU cap Windows enforces for every
//! thread of the process (and of the tools Atlas starts for previews and
//! conversions). Programs the user opens from Atlas break away silently, so
//! they are never throttled. Heavy worker threads additionally run in
//! background mode below 100%, which lowers their CPU, disk and memory
//! priority so the UI thread and other applications go first.
use std::sync::OnceLock;
use windows_sys::Win32::{
    Foundation::{GetLastError, HANDLE},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectCpuRateControlInformation,
            JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
            JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
        },
        Threading::{
            GetCurrentProcess, GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
            THREAD_MODE_BACKGROUND_END,
        },
    },
};

/// The process-wide job, created on first use. `None` when Windows refused it
/// (then the thread priorities still apply).
fn job() -> Option<HANDLE> {
    static JOB: OnceLock<Option<isize>> = OnceLock::new();
    *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job == 0 {
            return None;
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        // Child processes leave the job unless Atlas puts them in explicitly.
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
        let configured = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        ) != 0;
        // The handle lives for the whole process: closing it ends the limit.
        (configured && AssignProcessToJobObject(job, GetCurrentProcess()) != 0).then_some(job)
    })
}

/// Caps the CPU time of Atlas (and of contained tools) at `percent` of the
/// whole machine. 100 removes the cap. Takes effect immediately.
pub fn set_cpu_cap(percent: u8) -> std::io::Result<()> {
    let percent = u32::from(percent.clamp(1, 100));
    if percent == 100 && JOB_CREATED.get().is_none() {
        // Never create a job just to say "no limit".
        return Ok(());
    }
    let _ = JOB_CREATED.set(());
    let Some(job) = job() else {
        return Err(std::io::Error::other(
            "Windows did not allow a resource limit for this process",
        ));
    };
    unsafe {
        let mut rate: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = std::mem::zeroed();
        if percent < 100 {
            rate.ControlFlags =
                JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
            // Cycles per 10 000 across all processors.
            rate.Anonymous.CpuRate = percent * 100;
        }
        if SetInformationJobObject(
            job,
            JobObjectCpuRateControlInformation,
            std::ptr::addr_of!(rate).cast(),
            std::mem::size_of_val(&rate) as u32,
        ) == 0
        {
            return Err(std::io::Error::from_raw_os_error(GetLastError() as i32));
        }
    }
    Ok(())
}

static JOB_CREATED: OnceLock<()> = OnceLock::new();

/// Puts a tool Atlas just started (ffmpeg, ImageMagick, …) under the same
/// cap. `process` is a borrowed process handle. A no-op without a cap.
pub fn contain(process: isize) {
    if JOB_CREATED.get().is_none() {
        return;
    }
    if let Some(job) = job() {
        // SAFETY: both handles are valid; failure leaves the tool unlimited.
        unsafe { AssignProcessToJobObject(job, process) };
    }
}

/// Lower (or restore) the calling thread's CPU, disk and memory priority.
pub fn background_thread(on: bool) {
    let mode = if on {
        THREAD_MODE_BACKGROUND_BEGIN
    } else {
        THREAD_MODE_BACKGROUND_END
    };
    // SAFETY: the pseudo-handle names the calling thread. Calling END on a
    // thread that is not in background mode fails harmlessly.
    unsafe { SetThreadPriority(GetCurrentThread(), mode) };
}

#[cfg(test)]
mod tests {
    #[test]
    fn background_mode_round_trips_on_the_calling_thread() {
        std::thread::spawn(|| {
            super::background_thread(true);
            super::background_thread(false);
        })
        .join()
        .unwrap();
    }
}
