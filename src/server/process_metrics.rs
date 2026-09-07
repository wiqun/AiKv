//! 读取当前进程 /proc 指标 (Linux).

/// 时钟 tick 频率 (Linux 上通常为 100).
#[cfg(target_os = "linux")]
const USER_HZ: f64 = 100.0;

pub fn read_resident_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(kb) = line.strip_prefix("VmRSS:") {
                let kb: u64 = kb.split_whitespace().next()?.parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// 系统物理内存总量 (MemTotal), 单位字节.
pub fn read_total_system_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in meminfo.lines() {
            if let Some(kb) = line.strip_prefix("MemTotal:") {
                let kb: u64 = kb.split_whitespace().next()?.parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// 当前进程可执行文件路径.
pub fn read_executable_path() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link("/proc/self/exe")
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }
}

/// 累计 CPU 时间 user/system, 单位秒.
pub fn read_cpu_user_sys_seconds() -> Option<(f64, f64)> {
    #[cfg(target_os = "linux")]
    {
        read_cpu_jiffies().map(|(utime, stime)| (utime as f64 / USER_HZ, stime as f64 / USER_HZ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// 累计 CPU 时间 (user + system), 单位秒.
#[cfg(feature = "monitoring")]
pub fn read_cpu_seconds() -> Option<f64> {
    read_cpu_user_sys_seconds().map(|(user, sys)| user + sys)
}

#[cfg(target_os = "linux")]
fn read_cpu_jiffies() -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rparen = stat.rfind(')')?;
    let fields: Vec<&str> = stat[rparen + 2..].split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some((utime, stime))
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn read_cpu_jiffies() -> Option<(u64, u64)> {
    None
}

/// 累计磁盘读写字节 (read_bytes, write_bytes).
#[cfg(feature = "monitoring")]
pub fn read_io_bytes() -> Option<(u64, u64)> {
    #[cfg(target_os = "linux")]
    {
        let io = std::fs::read_to_string("/proc/self/io").ok()?;
        let mut read_bytes = None;
        let mut write_bytes = None;
        for line in io.lines() {
            if let Some(v) = line.strip_prefix("read_bytes:") {
                read_bytes = v.trim().parse().ok();
            } else if let Some(v) = line.strip_prefix("write_bytes:") {
                write_bytes = v.trim().parse().ok();
            }
        }
        Some((read_bytes?, write_bytes?))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// 纯函数: 解析 Linux /proc/self/status 文本中的 Threads 与上下文切换计数.
///
/// 严格采用行前缀匹配（不依赖字段在文件中的相对顺序或行号），
/// 畸形数字或溢出字符串经由 `parse::<u64>()` 安全返回 `None`，不触发 panic。
pub fn parse_status_threads_and_switches(status: &str) -> (Option<u64>, Option<u64>, Option<u64>) {
    let mut threads = None;
    let mut voluntary = None;
    let mut nonvoluntary = None;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Threads:") {
            threads = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("voluntary_ctxt_switches:") {
            voluntary = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("nonvoluntary_ctxt_switches:") {
            nonvoluntary = rest.trim().parse().ok();
        }
    }
    (threads, voluntary, nonvoluntary)
}

/// 进程线程数与上下文切换 (threads, voluntary, nonvoluntary).
///
/// 在 Linux 环境读取 `/proc/self/status` 并解析；非 Linux 环境或读取/解析失败时返回 `None`。
/// 上层调用方如 `ServerMetrics` 对 `None` 维持旧缓存或 0，与 `cached_rss_bytes` 保持一致兜底语义。
pub fn read_process_threads_and_context_switches() -> Option<(u64, u64, u64)> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let (t, v, nv) = parse_status_threads_and_switches(&status);
        Some((t?, v?, nv?))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_status_threads_and_switches_realistic() {
        let sample = "\
Name:\taikv-server
Umask:\t0022
State:\tS (sleeping)
Tgid:\t12345
Pid:\t12345
PPid:\t1
FDSize:\t256
VmPeak:\t   50000 kB
VmSize:\t   48000 kB
VmRSS:\t   12000 kB
Threads:\t24
SigQ:\t0/127110
SigPnd:\t0000000000000000
voluntary_ctxt_switches:\t1042
nonvoluntary_ctxt_switches:\t88
";
        let (threads, vol, nonvol) = parse_status_threads_and_switches(sample);
        assert_eq!(threads, Some(24));
        assert_eq!(vol, Some(1042));
        assert_eq!(nonvol, Some(88));
    }

    #[test]
    fn test_parse_status_threads_and_switches_shuffled_and_missing() {
        // 乱序与缺失行，验证不依赖特定行序与行号
        let sample = "\
nonvoluntary_ctxt_switches:\t999
OtherField:\tsomething
Threads:\t16
";
        let (threads, vol, nonvol) = parse_status_threads_and_switches(sample);
        assert_eq!(threads, Some(16));
        assert_eq!(vol, None);
        assert_eq!(nonvol, Some(999));
    }

    #[test]
    fn test_parse_status_threads_and_switches_overflow_and_malformed() {
        // 验证超长溢出字符串 (大于 u64::MAX)、负数、字母乱码等安全返回 None，禁止 panic
        let sample = "\
Threads:\t18446744073709551616
voluntary_ctxt_switches:\t-10
nonvoluntary_ctxt_switches:\tinvalid_number
";
        let (threads, vol, nonvol) = parse_status_threads_and_switches(sample);
        assert_eq!(threads, None, "u64 overflow must yield None");
        assert_eq!(vol, None, "negative integer must yield None");
        assert_eq!(nonvol, None, "malformed string must yield None");
    }

    #[test]
    fn test_parse_status_threads_and_switches_empty() {
        let (threads, vol, nonvol) = parse_status_threads_and_switches("");
        assert_eq!(threads, None);
        assert_eq!(vol, None);
        assert_eq!(nonvol, None);
    }
}
