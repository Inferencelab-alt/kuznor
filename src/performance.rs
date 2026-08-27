use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessSample {
    pub cpu_percent: Option<f32>,
    pub memory_bytes: Option<u64>,
}

#[derive(Debug, Default)]
pub struct ProcessSampler {
    previous_cpu_100ns: Option<u64>,
    previous_at: Option<Instant>,
}

impl ProcessSampler {
    pub fn sample(&mut self) -> ProcessSample {
        let Some((cpu_100ns, memory_bytes)) = current_process_usage() else {
            return ProcessSample::default();
        };
        let now = Instant::now();
        let cpu_percent = self.previous_cpu_100ns.zip(self.previous_at).and_then(
            |(previous_cpu, previous_at)| {
                let elapsed = now.duration_since(previous_at).as_secs_f32();
                if elapsed <= 0.0 || cpu_100ns < previous_cpu {
                    return None;
                }
                let logical_processors = std::thread::available_parallelism()
                    .map(|count| count.get() as f32)
                    .unwrap_or(1.0);
                let cpu_seconds = (cpu_100ns - previous_cpu) as f32 / 10_000_000.0;
                Some((cpu_seconds / elapsed / logical_processors * 100.0).clamp(0.0, 100.0))
            },
        );
        self.previous_cpu_100ns = Some(cpu_100ns);
        self.previous_at = Some(now);
        ProcessSample {
            cpu_percent,
            memory_bytes: Some(memory_bytes),
        }
    }
}

#[cfg(windows)]
fn current_process_usage() -> Option<(u64, u64)> {
    use std::{ffi::c_void, mem::size_of};

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }

    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCounters,
            size: u32,
        ) -> i32;
    }

    fn file_time_value(time: &FileTime) -> u64 {
        (u64::from(time.high) << 32) | u64::from(time.low)
    }

    let process = unsafe { GetCurrentProcess() };
    let mut creation = FileTime::default();
    let mut exit = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    let times_ok =
        unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) } != 0;
    let mut counters = ProcessMemoryCounters {
        cb: size_of::<ProcessMemoryCounters>() as u32,
        ..Default::default()
    };
    let memory_ok = unsafe {
        GetProcessMemoryInfo(
            process,
            &mut counters,
            size_of::<ProcessMemoryCounters>() as u32,
        )
    } != 0;
    if times_ok && memory_ok {
        Some((
            file_time_value(&kernel) + file_time_value(&user),
            counters.working_set_size as u64,
        ))
    } else {
        None
    }
}

#[cfg(not(windows))]
fn current_process_usage() -> Option<(u64, u64)> {
    None
}

pub fn format_memory(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    format!("{:.1} MB", bytes as f64 / MB)
}

pub fn should_sample(last_sample: Instant) -> bool {
    last_sample.elapsed() >= Duration::from_secs(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_format_is_local_and_stable() {
        assert_eq!(format_memory(1024 * 1024), "1.0 MB");
    }
}
