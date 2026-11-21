use crate::{Choice, Profile};
use rustix::time::{ClockId, clock_gettime};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs,
    path::{Component, Path},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Default, Serialize)]
pub(crate) struct Usage {
    pub uptime_seconds: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub ram_bytes: Option<u64>,
    pub memory_current_bytes: Option<u64>,
    pub swap_bytes: Option<u64>,
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
    pub fn snapshot(profiles: &[Profile], entries: &[Choice]) -> Vec<Usage> {
        let mut monitor = Self::default();
        let initial = monitor.sample(profiles, entries);
        if monitor.cpu.is_empty() {
            return initial;
        }
        // The JSON command has no prior sample. Take a short CPU measurement
        // so it remains responsive when used to populate a launcher menu.
        std::thread::sleep(Duration::from_millis(200));
        monitor.sample(profiles, entries)
    }

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
                usage.uptime_seconds = counter("ActiveEnterTimestampMonotonic")
                    .filter(|&started| started != 0)
                    .and_then(|started| since_boot.checked_sub(Duration::from_micros(started)))
                    .map(|uptime| uptime.as_secs());
                if let Some((current, working_set)) = properties
                    .get("ControlGroup")
                    .and_then(|path| read_memory_usage(path))
                {
                    usage.memory_current_bytes = Some(current);
                    usage.ram_bytes = Some(working_set);
                }
                usage.swap_bytes = counter("MemorySwapCurrent");
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

fn read_memory_usage(cgroup: &str) -> Option<(u64, u64)> {
    let path = Path::new(cgroup.strip_prefix('/')?);
    if path.as_os_str().is_empty()
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let path = Path::new("/sys/fs/cgroup").join(path);
    let current = fs::read_to_string(path.join("memory.current"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    let working_set = fs::read_to_string(path.join("memory.stat"))
        .map_or(current, |stat| memory_without_inactive_file(current, &stat));
    Some((current, working_set))
}

fn memory_without_inactive_file(current: u64, stat: &str) -> u64 {
    // Match docker stats on cgroup v2. The two files can change between reads,
    // so discount inactive file pages only when their count is below the total.
    stat.lines()
        .find_map(|line| line.strip_prefix("inactive_file ")?.parse::<u64>().ok())
        .filter(|&inactive| inactive < current)
        .map_or(current, |inactive| current - inactive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_set_discounts_only_inactive_file_pages() {
        let stat = "file 16384\nactive_file 8192\ninactive_file 4096\nunevictable 4096\n";
        assert_eq!(memory_without_inactive_file(20_480, stat), 16_384);
        assert_eq!(
            memory_without_inactive_file(4096, "active_file 4096\n"),
            4096
        );
        assert_eq!(
            memory_without_inactive_file(4096, "inactive_file 4096\n"),
            4096
        );
        assert_eq!(
            memory_without_inactive_file(4096, "inactive_file 8192\n"),
            4096
        );
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
