//! Process priority and CPU affinity management for real-time performance

/// Set high priority and optionally CPU affinity for the current thread
pub fn set_high_priority(enable_cpu_affinity: bool) {
    #[cfg(target_os = "linux")]
    {
        use libc::{
            CPU_SET, CPU_ZERO, PRIO_PROCESS, SCHED_FIFO, cpu_set_t, getpid, pthread_self,
            pthread_setaffinity_np, sched_setaffinity, setpriority,
        };
        use std::mem;

        unsafe {
            // Optionally set CPU affinity to a random core
            if enable_cpu_affinity {
                let num_cpus = num_cpus::get();

                // Randomly pick a CPU core
                use std::time::{SystemTime, UNIX_EPOCH};
                let seed = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as u64;
                let target_cpu = (seed % num_cpus as u64) as usize;

                let mut cpuset: cpu_set_t = mem::zeroed();
                CPU_ZERO(&mut cpuset);
                CPU_SET(target_cpu, &mut cpuset);

                // Try pthread_setaffinity_np first (thread-specific)
                let result =
                    pthread_setaffinity_np(pthread_self(), mem::size_of::<cpu_set_t>(), &cpuset);
                if result == 0 {
                    log::info!("Set CPU affinity to randomly selected core {}", target_cpu);
                } else {
                    // Fallback to sched_setaffinity (process-wide)
                    let result = sched_setaffinity(0, mem::size_of::<cpu_set_t>(), &cpuset);
                    if result == 0 {
                        log::info!(
                            "Set CPU affinity to randomly selected core {} (process-wide)",
                            target_cpu
                        );
                    } else {
                        log::warn!("Failed to set CPU affinity");
                    }
                }
            } else {
                log::debug!("CPU affinity disabled - letting OS handle CPU scheduling");
            }

            // Set high process priority (nice value)
            let result = setpriority(PRIO_PROCESS, getpid() as u32, -10);
            if result == 0 {
                log::info!("Set high process priority (nice -10)");
            } else {
                log::warn!("Failed to set high process priority");
            }

            // Attempt to set real-time scheduling (requires root)
            let param = libc::sched_param { sched_priority: 50 };
            let result = libc::sched_setscheduler(0, SCHED_FIFO, &param);
            if result == 0 {
                log::info!("Set real-time scheduling (SCHED_FIFO, priority 50)");
            } else {
                log::debug!("Could not set real-time scheduling (requires root privileges)");
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        log::warn!("Priority setting is only supported on Linux");
        let _ = enable_cpu_affinity; // Suppress unused parameter warning
    }
}
