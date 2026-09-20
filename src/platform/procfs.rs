//! Thin readers for `/proc` and `statvfs`.
//!
//! Every function returns `Option`/`Result` instead of panicking: a missing
//! file (containers, hardened kernels) must only degrade one widget, never the
//! whole dashboard.

use std::fs;
use std::path::Path;
use std::time::Duration;

use log::debug;
use rustix::fs::statvfs;

/// Cumulative CPU jiffies of the whole machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub total: u64,
    pub idle: u64,
}

impl CpuTimes {
    /// Busy percentage between two samples, or `None` if time did not advance.
    pub fn usage_since(self, previous: Self) -> Option<f64> {
        let total = self.total.checked_sub(previous.total)?;
        let idle = self.idle.saturating_sub(previous.idle);
        if total == 0 {
            return None;
        }
        let busy = total.saturating_sub(idle);
        Some((busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
    }
}

/// Memory counters, in bytes.
#[derive(Debug, Clone, Copy, Default)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

impl Memory {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }
}

/// Aggregate CPU counters from `/proc/stat`.
pub fn cpu_times() -> Option<CpuTimes> {
    cpu_times_per_core().into_iter().next()
}

/// CPU counters per logical core, index 0 being the machine total.
pub fn cpu_times_per_core() -> Vec<CpuTimes> {
    let Some(raw) = read("/proc/stat") else {
        return Vec::new();
    };
    raw.lines()
        .take_while(|line| line.starts_with("cpu"))
        .filter_map(parse_cpu_line)
        .collect()
}

fn parse_cpu_line(line: &str) -> Option<CpuTimes> {
    let mut fields = line
        .split_whitespace()
        .skip(1)
        .filter_map(|f| f.parse::<u64>().ok());
    let user = fields.next()?;
    let nice = fields.next().unwrap_or(0);
    let system = fields.next().unwrap_or(0);
    let idle = fields.next().unwrap_or(0);
    let iowait = fields.next().unwrap_or(0);
    let irq = fields.next().unwrap_or(0);
    let softirq = fields.next().unwrap_or(0);
    let steal = fields.next().unwrap_or(0);

    let idle_all = idle + iowait;
    let total = user + nice + system + irq + softirq + steal + idle_all;
    Some(CpuTimes {
        total,
        idle: idle_all,
    })
}

/// Number of logical CPUs.
pub fn cpu_count() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// Model name of the first CPU.
pub fn cpu_model() -> Option<String> {
    let raw = read("/proc/cpuinfo")?;
    raw.lines()
        .find_map(|line| line.strip_prefix("model name"))
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, value)| value.trim().to_owned())
}

/// Memory and swap counters from `/proc/meminfo`.
pub fn memory() -> Option<Memory> {
    let raw = read("/proc/meminfo")?;
    let mut mem = Memory::default();
    for line in raw.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let kib = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        match key {
            "MemTotal" => mem.total = kib * 1024,
            "MemAvailable" => mem.available = kib * 1024,
            "SwapTotal" => mem.swap_total = kib * 1024,
            "SwapFree" => mem.swap_free = kib * 1024,
            _ => {}
        }
    }
    (mem.total > 0).then_some(mem)
}

/// 1/5/15 minute load averages.
pub fn load_average() -> Option<[f64; 3]> {
    let raw = read("/proc/loadavg")?;
    let mut fields = raw.split_whitespace();
    let one = fields.next()?.parse().ok()?;
    let five = fields.next()?.parse().ok()?;
    let fifteen = fields.next()?.parse().ok()?;
    Some([one, five, fifteen])
}

/// Seconds since boot.
pub fn uptime() -> Option<Duration> {
    let raw = read("/proc/uptime")?;
    let seconds: f64 = raw.split_whitespace().next()?.parse().ok()?;
    Some(Duration::from_secs_f64(seconds.max(0.0)))
}

/// Kernel release, e.g. `6.12.1-arch1-1`.
pub fn kernel_release() -> Option<String> {
    read("/proc/sys/kernel/osrelease").map(|s| s.trim().to_owned())
}

/// Machine hostname.
pub fn hostname() -> Option<String> {
    read("/proc/sys/kernel/hostname").map(|s| s.trim().to_owned())
}

/// Bytes received and transmitted by one network interface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetCounters {
    pub received: u64,
    pub transmitted: u64,
}

impl NetCounters {
    /// Bytes per second since an earlier sample.
    pub fn rates_since(self, previous: Self, seconds: f64) -> (f64, f64) {
        if seconds <= 0.0 {
            return (0.0, 0.0);
        }
        let down = self.received.saturating_sub(previous.received) as f64 / seconds;
        let up = self.transmitted.saturating_sub(previous.transmitted) as f64 / seconds;
        (down, up)
    }
}

/// Traffic counters of every interface in `/proc/net/dev`, loopback excluded.
pub fn network_counters() -> Vec<(String, NetCounters)> {
    let Some(raw) = read("/proc/net/dev") else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let name = name.trim();
            if name.is_empty() || name == "lo" {
                return None;
            }
            let mut fields = rest
                .split_whitespace()
                .filter_map(|f| f.parse::<u64>().ok());
            let received = fields.next()?;
            let transmitted = fields.nth(7)?;
            Some((
                name.to_owned(),
                NetCounters {
                    received,
                    transmitted,
                },
            ))
        })
        .collect()
}

/// Interface carrying the default route, from `/proc/net/route`.
pub fn default_interface() -> Option<String> {
    let raw = read("/proc/net/route")?;
    raw.lines().skip(1).find_map(|line| {
        let mut fields = line.split_whitespace();
        let name = fields.next()?;
        let destination = fields.next()?;
        (destination == "00000000").then(|| name.to_owned())
    })
}

/// A real filesystem currently mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub device: String,
    pub point: String,
    pub fstype: String,
}

/// Pseudo filesystems, which are never interesting in a dashboard.
const PSEUDO_FILESYSTEMS: [&str; 22] = [
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "tmpfs",
    "cgroup",
    "cgroup2",
    "overlay",
    "squashfs",
    "efivarfs",
    "ramfs",
    "mqueue",
    "hugetlbfs",
    "configfs",
    "debugfs",
    "tracefs",
    "securityfs",
    "pstore",
    "bpf",
    "autofs",
    "nsfs",
    "rpc_pipefs",
];

/// Mounted real filesystems, entry points deduplicated and sorted by path.
pub fn mounts() -> Vec<Mount> {
    let Some(raw) = read("/proc/mounts") else {
        return Vec::new();
    };
    let mut mounts: Vec<Mount> = raw
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let device = unescape(fields.next()?);
            let point = unescape(fields.next()?);
            let fstype = fields.next()?.to_owned();
            if PSEUDO_FILESYSTEMS.contains(&fstype.as_str()) {
                return None;
            }
            let is_device = device.starts_with("/dev/");
            let is_network = fstype.starts_with("nfs")
                || fstype.starts_with("cifs")
                || fstype.starts_with("fuse.sshfs")
                || fstype == "zfs"
                || fstype == "bcachefs";
            (is_device || is_network).then_some(Mount {
                device,
                point,
                fstype,
            })
        })
        .collect();

    mounts.sort_by(|a, b| a.point.cmp(&b.point));
    mounts.dedup_by(|a, b| a.point == b.point);
    mounts
}

/// `/proc/mounts` escapes spaces and friends as octal sequences.
fn unescape(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        let digits: String = chars.by_ref().take(3).collect();
        match u8::from_str_radix(&digits, 8) {
            Ok(byte) => out.push(char::from(byte)),
            Err(_) => out.push_str(&digits),
        }
    }
    out
}

/// Disk usage of the filesystem containing `path`.
pub fn disk_usage(path: &str) -> Option<(u64, u64)> {
    let stat = match statvfs(path) {
        Ok(stat) => stat,
        Err(e) => {
            debug!("statvfs({path}) 失敗: {e}");
            return None;
        }
    };
    let unit = stat.f_frsize.max(stat.f_bsize).max(1);
    let total = stat.f_blocks.saturating_mul(unit);
    let free = stat.f_bavail.saturating_mul(unit);
    (total > 0).then(|| (total, total.saturating_sub(free)))
}

/// `/proc/diskstats` counts in 512 byte sectors, whatever the device reports.
const SECTOR_SIZE: u64 = 512;

/// Cumulative I/O of one block device, from `/proc/diskstats`.
#[derive(Debug, Clone)]
pub struct DiskIo {
    pub name: String,
    /// Bytes read since boot, as counted by `/proc/diskstats` in 512 byte sectors.
    pub read_bytes: u64,
    pub write_bytes: u64,
}

/// Every whole block device, partitions excluded so nothing is counted twice.
/// Pseudo devices (`loop*`, `ram*`, `zram*`) are skipped. Sorted by name.
pub fn disk_io() -> Vec<DiskIo> {
    let Some(raw) = read("/proc/diskstats") else {
        return Vec::new();
    };
    let mut disks: Vec<DiskIo> = raw
        .lines()
        .filter_map(parse_disk_line)
        .filter_map(|(name, read_sectors, write_sectors)| {
            is_whole_device(&name).then_some(DiskIo {
                name,
                read_bytes: read_sectors.saturating_mul(SECTOR_SIZE),
                write_bytes: write_sectors.saturating_mul(SECTOR_SIZE),
            })
        })
        .collect();
    disks.sort_by(|a, b| a.name.cmp(&b.name));
    disks
}

/// Name and 512 byte sector counters of one `/proc/diskstats` line.
fn parse_disk_line(line: &str) -> Option<(String, u64, u64)> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    // 1 major, 2 minor, 3 name, 4 reads, 5 merged, 6 sectors read,
    // 7 ms reading, 8 writes, 9 merged, 10 sectors written.
    if fields.len() < 10 {
        return None;
    }
    let name = fields[2].to_owned();
    let read_sectors = fields[5].parse::<u64>().ok()?;
    let write_sectors = fields[9].parse::<u64>().ok()?;
    Some((name, read_sectors, write_sectors))
}

/// A whole disk has a `/sys/block` entry; a partition only a subdirectory of its
/// parent, so this is what keeps the two from being added up twice.
fn is_whole_device(name: &str) -> bool {
    let pseudo = name.starts_with("loop") || name.starts_with("ram") || name.starts_with("zram");
    !pseudo && Path::new("/sys/block").join(name).exists()
}

/// One process, from `/proc/<pid>`.
#[derive(Debug, Clone)]
pub struct Process {
    pub pid: i32,
    /// Command name as the kernel reports it, at most 15 characters.
    pub name: String,
    /// Resident memory in bytes.
    pub rss: u64,
    /// CPU time (user + system) in clock ticks. Sampling twice and taking the
    /// difference yields the load in between; same unit as [`CpuTimes::total`].
    pub cpu_ticks: u64,
}

/// Every readable process, unsorted. Pids that vanish while reading are skipped.
pub fn processes() -> Vec<Process> {
    let Ok(entries) = fs::read_dir("/proc") else {
        debug!("/proc を読めません");
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let pid = entry.ok()?.file_name().to_str()?.parse::<i32>().ok()?;
            read_process(pid)
        })
        .collect()
}

fn read_process(pid: i32) -> Option<Process> {
    let stat = read(&format!("/proc/{pid}/stat"))?;
    let (pid, name, cpu_ticks) = parse_stat_line(&stat)?;
    // Kernel threads have no resident set at all, hence no `VmRSS` line.
    let rss = read(&format!("/proc/{pid}/status")).map_or(0, |status| parse_status_vm_rss(&status));
    Some(Process {
        pid,
        name,
        rss,
        cpu_ticks,
    })
}

/// Pid, command name and CPU ticks of one `/proc/<pid>/stat` line.
fn parse_stat_line(line: &str) -> Option<(i32, String, u64)> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close < open {
        return None;
    }
    let pid = line[..open].trim().parse::<i32>().ok()?;
    // `comm` may contain spaces and parentheses, so it is delimited by the last
    // `)` rather than split on whitespace.
    let name = line[open + 1..close].to_owned();
    // The state field follows the `)`, which makes `utime` (field 14) index 11
    // and `stime` (field 15) index 12 of the remainder.
    let mut fields = line[close + 1..].split_whitespace();
    let _state = fields.next()?;
    let utime = fields.nth(10)?.parse::<u64>().ok()?;
    let stime = fields.next()?.parse::<u64>().ok()?;
    Some((pid, name, utime.saturating_add(stime)))
}

/// Resident memory in bytes of a `/proc/<pid>/status` file, 0 if it has no line.
fn parse_status_vm_rss(text: &str) -> u64 {
    text.lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kib| kib.parse::<u64>().ok())
        .map_or(0, |kib| kib.saturating_mul(1024))
}

fn read(path: &str) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(raw) => Some(raw),
        Err(e) => {
            debug!("{path} を読めません: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_times_are_monotonic() {
        let Some(first) = cpu_times() else {
            return;
        };
        std::thread::sleep(Duration::from_millis(20));
        let Some(second) = cpu_times() else {
            return;
        };
        assert!(second.total >= first.total);
        if let Some(usage) = second.usage_since(first) {
            assert!((0.0..=100.0).contains(&usage));
        }
    }

    #[test]
    fn memory_is_reported() {
        let Some(mem) = memory() else {
            return;
        };
        assert!(mem.total > 0);
        assert!(mem.used() <= mem.total);
    }

    #[test]
    fn load_average_parses() {
        if let Some([one, _, _]) = load_average() {
            assert!(one >= 0.0);
        }
    }

    #[test]
    fn per_core_times_start_with_the_total() {
        let cores = cpu_times_per_core();
        if cores.is_empty() {
            return;
        }
        assert!(
            cores.len() >= 2,
            "expected the total plus at least one core"
        );
        assert!(cores[0].total >= cores[1].total);
    }

    #[test]
    fn mounts_are_real_filesystems() {
        for mount in mounts() {
            assert!(mount.point.starts_with('/'));
            assert!(!PSEUDO_FILESYSTEMS.contains(&mount.fstype.as_str()));
        }
    }

    #[test]
    fn octal_escapes_are_decoded() {
        assert_eq!(unescape("/media/My\\040Disk"), "/media/My Disk");
        assert_eq!(unescape("/home"), "/home");
    }

    #[test]
    fn diskstats_lines_are_parsed() {
        let line = "   8       0 sda 12345 6 67890 100 234 7 456 200 0 100 300";
        let (name, read, written) = parse_disk_line(line).expect("a full line must parse");
        assert_eq!(name, "sda");
        assert_eq!(read, 67890);
        assert_eq!(written, 456);
    }

    #[test]
    fn broken_diskstats_lines_are_rejected() {
        assert!(parse_disk_line("").is_none());
        assert!(parse_disk_line("   ").is_none());
        assert!(parse_disk_line("8 0 sda 1 2 3").is_none(), "too short");
        assert!(
            parse_disk_line("8 0 sda 1 2 not-a-number 4 5 6 7").is_none(),
            "sectors must be numeric"
        );
    }

    #[test]
    fn whole_devices_exclude_partitions_and_pseudo_devices() {
        // None of these exist as real whole disks in a container, but the names
        // must still be rejected before the filesystem is consulted.
        assert!(!is_whole_device("loop0"));
        assert!(!is_whole_device("ram3"));
        assert!(!is_whole_device("zram0"));
        assert!(!is_whole_device("definitely-not-a-disk"));
    }

    #[test]
    fn disk_io_is_sorted_and_counted_in_bytes() {
        let disks = disk_io();
        for pair in disks.windows(2) {
            assert!(pair[0].name <= pair[1].name, "names must be sorted");
        }
        for disk in &disks {
            assert!(!disk.name.is_empty());
            assert!(is_whole_device(&disk.name));
            assert_eq!(disk.read_bytes % SECTOR_SIZE, 0);
            assert_eq!(disk.write_bytes % SECTOR_SIZE, 0);
        }
    }

    #[test]
    fn stat_lines_are_parsed_with_awkward_names() {
        // 11 -> utime, 12 -> stime of the fields following the closing parenthesis.
        let line = "4242 (my (weird) proc) S 1 2 3 4 5 6 7 8 9 10 11 12 13";
        let (pid, name, ticks) = parse_stat_line(line).expect("a full line must parse");
        assert_eq!(pid, 4242);
        assert_eq!(name, "my (weird) proc");
        assert_eq!(ticks, 23);
    }

    #[test]
    fn stat_lines_with_plain_names_are_parsed() {
        let line = "1 (systemd) S 0 1 1 0 -1 4194560 100 0 0 0 11 12 0 0";
        let (pid, name, ticks) = parse_stat_line(line).expect("a full line must parse");
        assert_eq!(pid, 1);
        assert_eq!(name, "systemd");
        assert_eq!(ticks, 23);
    }

    #[test]
    fn broken_stat_lines_are_rejected() {
        assert!(parse_stat_line("").is_none());
        assert!(parse_stat_line("no parenthesis at all").is_none());
        assert!(parse_stat_line("1 (systemd").is_none(), "unterminated comm");
        assert!(parse_stat_line("1 (systemd) S 0 1").is_none(), "truncated");
        assert!(parse_stat_line("x (systemd) S 0 1 1 0 -1 0 0 0 0 0 1 2").is_none());
    }

    #[test]
    fn vm_rss_is_read_from_status() {
        let status = "Name:\tfirefox\nState:\tS (sleeping)\n\nVmPeak:\t 900000 kB\n\
                      VmRSS:\t  123456 kB\nVmHWM:\t 234567 kB\n";
        assert_eq!(parse_status_vm_rss(status), 123_456 * 1024);
        assert_eq!(parse_status_vm_rss("Name:\tcpuhp/0\nState:\tI (idle)\n"), 0);
        assert_eq!(parse_status_vm_rss(""), 0);
        assert_eq!(parse_status_vm_rss("VmRSS:\tgarbage kB\n"), 0);
    }

    #[test]
    fn processes_are_readable_and_include_ourselves() {
        let all = processes();
        if all.is_empty() {
            return;
        }
        for process in &all {
            assert!(process.pid > 0);
            assert!(!process.name.is_empty());
        }
        let me = std::process::id() as i32;
        let ours = all.iter().find(|p| p.pid == me).expect("own process");
        assert!(!ours.name.is_empty());
        assert_eq!(ours.rss % 1024, 0, "VmRSS is reported in kibibytes");
    }

    #[test]
    fn process_ticks_are_monotonic() {
        let me = std::process::id() as i32;
        let Some(first) = processes().into_iter().find(|p| p.pid == me) else {
            return;
        };
        std::thread::sleep(Duration::from_millis(20));
        let Some(second) = processes().into_iter().find(|p| p.pid == me) else {
            return;
        };
        assert!(second.cpu_ticks >= first.cpu_ticks);
    }
}
