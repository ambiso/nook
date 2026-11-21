use crate::{Choice, Profile};
use rustix::time::{ClockId, clock_gettime};
use std::{
    collections::HashMap,
    fs,
    path::{Component, Path},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(crate) struct Usage {
    pub uptime: Option<Duration>,
    pub cpu_percent: Option<f64>,
    pub ram: Option<u64>,
    pub swap: Option<u64>,
}

struct CpuSample {
    invocation: String,
    time: Instant,
    cpu_ns: u64,
}

impl CpuSample {
    fn percent_since(&self, previous: &Self) -> Option<f64> {
        if self.invocation != previous.invocation {
            return None;
        }
        let elapsed = self.time.checked_duration_since(previous.time)?;
        let cpu_ns = self.cpu_ns.checked_sub(previous.cpu_ns)?;
        if elapsed.is_zero() {
            return None;
        }
        // One fully occupied core uses one second of CPU time per second.
        Some(100.0 * Duration::from_nanos(cpu_ns).as_secs_f64() / elapsed.as_secs_f64())
    }
}

#[derive(Default)]
pub(crate) struct UsageMonitor {
    cpu: HashMap<String, CpuSample>,
}

impl UsageMonitor {
    pub fn sample(&mut self, profiles: &[Profile], entries: &[Choice]) -> Vec<Usage> {
        let output = if entries.iter().any(|entry| entry.running) {
            // Query all active units together so refresh costs one process, not one per row.
            Command::new("systemctl")
                .args([
                    "show",
                    "--no-pager",
                    "--property=Id,ActiveState,ActiveEnterTimestampMonotonic,InvocationID,CPUUsageNSec,ControlGroup,MemorySwapCurrent",
                    "sandbox-*@*.service",
                ])
                .output()
                .ok()
                .filter(|output| output.status.success())
        } else {
            None
        };
        let time = Instant::now();
        let now = clock_gettime(ClockId::Monotonic);
        let since_boot = Duration::new(now.tv_sec as u64, now.tv_nsec as u32);
        let output = output
            .as_ref()
            .map(|output| String::from_utf8_lossy(&output.stdout));
        let units: HashMap<_, _> = output
            .as_deref()
            .unwrap_or_default()
            .split("\n\n")
            .filter_map(|block| {
                let properties: HashMap<_, _> = block
                    .lines()
                    .filter_map(|line| line.split_once('='))
                    .collect();
                Some((*properties.get("Id")?, properties))
            })
            .collect();

        let previous = std::mem::take(&mut self.cpu);
        entries
            .iter()
            .map(|entry| {
                let mut usage = Usage::default();
                if !entry.running {
                    return usage;
                }
                let Ok(unit) = profiles[entry.profile].unit(&entry.instance) else {
                    return usage;
                };
                let Some(properties) = units.get(unit.as_str()) else {
                    return usage;
                };
                if properties.get("ActiveState") != Some(&"active") {
                    return usage;
                }
                let counter = |key| {
                    properties
                        .get(key)?
                        .parse::<u64>()
                        .ok()
                        .filter(|&value| value != u64::MAX)
                };
                usage.uptime = counter("ActiveEnterTimestampMonotonic")
                    .filter(|&started| started != 0)
                    .and_then(|started| since_boot.checked_sub(Duration::from_micros(started)));
                usage.ram = properties
                    .get("ControlGroup")
                    .and_then(|path| read_resident_ram(path));
                usage.swap = counter("MemorySwapCurrent");
                if let (Some(&invocation), Some(cpu_ns)) =
                    (properties.get("InvocationID"), counter("CPUUsageNSec"))
                    && !invocation.is_empty()
                {
                    let sample = CpuSample {
                        invocation: invocation.into(),
                        time,
                        cpu_ns,
                    };
                    usage.cpu_percent = previous
                        .get(&unit)
                        .and_then(|previous| sample.percent_since(previous));
                    self.cpu.insert(unit, sample);
                }
                usage
            })
            .collect()
    }
}

fn read_resident_ram(cgroup: &str) -> Option<u64> {
    let path = Path::new(cgroup.strip_prefix('/')?);
    if path.as_os_str().is_empty()
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let stat =
        fs::read_to_string(Path::new("/sys/fs/cgroup").join(path).join("memory.stat")).ok()?;
    resident_ram(&stat)
}

fn resident_ram(stat: &str) -> Option<u64> {
    let counter = |key| {
        stat.lines().find_map(|line| {
            let (name, value) = line.split_once(' ')?;
            (name == key).then(|| value.parse::<u64>().ok()).flatten()
        })
    };
    // shmem is counted under file, alongside disk cache. Include shmem itself so
    // tmpfs and shared mappings count without including ordinary file pages.
    counter("anon")?.checked_add(counter("shmem")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_ram_excludes_disk_cache_but_counts_shared_memory() {
        let stat = "file 409600\nkernel 4096\nshmem 4096\nfile_mapped 8192\nanon 16384\n";
        assert_eq!(resident_ram(stat), Some(20480));
        assert_eq!(resident_ram("anon 0\nshmem 0\n"), Some(0));
        assert_eq!(resident_ram("anon 16384\n"), None);
    }

    #[test]
    fn cpu_samples_do_not_cross_restarts() {
        let previous = CpuSample {
            invocation: "old".into(),
            time: Instant::now(),
            cpu_ns: 1_000_000_000,
        };
        let mut current = CpuSample {
            invocation: "old".into(),
            time: previous.time + Duration::from_secs(2),
            cpu_ns: 4_000_000_000,
        };
        assert_eq!(current.percent_since(&previous), Some(150.0));
        current.invocation = "new".into();
        assert_eq!(current.percent_since(&previous), None);
    }
}
