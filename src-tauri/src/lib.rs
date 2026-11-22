use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::sync::{mpsc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

// ─── Data Types ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PortEntry {
    pub port: u16,
    pub pid: u32,
    pub pids: Vec<u32>,
    pub process_name: String,
    pub cmdline: String,
    pub cwd: String,
    pub state: String,
    pub cpu_percent: f32,
    pub mem_mb: f32,
    pub uptime_seconds: u64,
    pub uid: u32,
    pub is_protected: bool,
}

fn append_log(message: &str) {
    #[cfg(debug_assertions)]
    {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/harbor.log")
        {
            use std::io::Write;
            let _ = writeln!(file, "{message}");
        }
    }
    let _ = message;
}

// ─── Process stats from /proc ─────────────────────────────────────────────────

fn get_process_name(pid: u32) -> String {
    fs::read_to_string(format!("/proc/{}/comm", pid))
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn basename_from_path(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .trim()
        .to_string()
}

fn is_generic_thread_name(name: &str) -> bool {
    matches!(
        name,
        "MainThread"
            | "main"
            | "ThreadPoolForeg"
            | "ThreadPoolBackg"
            | "Chrome_IOThread"
            | "Chrome_ChildIOT"
            | "Compositor"
            | "VizCompositorTh"
            | "CPUThread"
            | "MemoryInfra"
            | "Broker"
            | "unknown"
            | ""
    ) || name.starts_with("ThreadPool")
        || name.starts_with("Chrome_")
        || name.ends_with("Thread")
}

fn package_name_from_script_path(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }

    if let Some(idx) = parts.iter().position(|p| *p == "node_modules") {
        if let Some(pkg) = parts.get(idx + 1) {
            if *pkg == ".bin" {
                if let Some(bin) = parts.get(idx + 2) {
                    return Some((*bin).to_string());
                }
            } else if !pkg.starts_with('.') {
                return Some((*pkg).to_string());
            }
        }
    }

    // .../openclaw/dist/index.js → openclaw
    if parts.len() >= 3 {
        let parent = parts[parts.len() - 2];
        if matches!(parent, "dist" | "build" | "lib" | "out" | ".next") {
            let pkg = parts[parts.len() - 3];
            if !pkg.starts_with('.') && pkg != "node_modules" {
                return Some(pkg.to_string());
            }
        }
    }

    None
}

fn cmdline_display_name(cmdline: &str) -> Option<String> {
    let lower = cmdline.to_lowercase();

    // Prefer framework names when present in the command line / process title.
    if lower.contains("next-server")
        || lower.starts_with("next-server")
        || lower.contains("next start")
        || lower.contains("next dev")
        || lower.contains("/next/")
        || lower.contains(" next ")
    {
        return Some("next".to_string());
    }
    if lower.contains("vite") {
        return Some("vite".to_string());
    }
    if lower.contains("nuxt") {
        return Some("nuxt".to_string());
    }

    let first = cmdline.split_whitespace().next()?.trim();
    if first.is_empty() {
        return None;
    }

    let base = basename_from_path(first);
    if base.is_empty() || is_generic_thread_name(&base) {
        return None;
    }

    let base_lower = base.to_lowercase();
    if matches!(
        base_lower.as_str(),
        "node" | "nodejs" | "python" | "python3" | "ruby" | "perl" | "php" | "java" | "deno" | "bun"
    ) {
        for token in cmdline.split_whitespace().skip(1) {
            if token.starts_with('-') {
                continue;
            }
            if let Some(pkg) = package_name_from_script_path(token) {
                if !is_generic_thread_name(&pkg) {
                    return Some(pkg);
                }
            }
            let script = basename_from_path(token);
            if script.is_empty() || is_generic_thread_name(&script) {
                continue;
            }
            if !script.contains('.') {
                return Some(script);
            }
        }
        return Some(base);
    }

    // next-server (v16.2.7) style process titles
    if base_lower.starts_with("next-server") {
        return Some("next".to_string());
    }

    Some(base)
}

/// Prefer a human-meaningful name over Chromium/Node thread labels like "MainThread".
fn resolve_display_name(pid: u32, ss_name: &str, cmdline: &str) -> String {
    let ss_trimmed = ss_name.trim();
    if ss_trimmed.to_lowercase().starts_with("next-server") {
        return "next".to_string();
    }

    let candidates = [
        cmdline_display_name(cmdline),
        {
            if !ss_trimmed.is_empty() && !is_generic_thread_name(ss_trimmed) {
                Some(ss_trimmed.to_string())
            } else {
                None
            }
        },
        {
            let comm = if pid > 0 {
                get_process_name(pid)
            } else {
                String::new()
            };
            if comm.to_lowercase().starts_with("next-server") {
                Some("next".to_string())
            } else if !comm.is_empty() && !is_generic_thread_name(&comm) {
                Some(comm)
            } else {
                None
            }
        },
    ];

    for candidate in candidates.into_iter().flatten() {
        if !candidate.is_empty() {
            return candidate;
        }
    }

    if !ss_trimmed.is_empty() {
        return ss_trimmed.to_string();
    }

    let comm = if pid > 0 {
        get_process_name(pid)
    } else {
        String::new()
    };
    if !comm.is_empty() {
        return comm;
    }

    "unknown".to_string()
}

fn get_cmdline(pid: u32) -> String {
    let raw = fs::read(format!("/proc/{}/cmdline", pid)).unwrap_or_default();
    let s: String = raw
        .iter()
        .map(|&b| if b == 0 { ' ' } else { b as char })
        .collect();
    let trimmed = s.trim().to_string();
    if !trimmed.is_empty() {
        return trimmed;
    }

    // Some runtimes (Next.js) rewrite the process title and leave cmdline empty.
    // Fall back to comm so we can still identify next-server / node workers.
    get_process_name(pid)
}

fn get_process_cwd(pid: u32) -> String {
    fs::read_link(format!("/proc/{}/cwd", pid))
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn get_process_exe(pid: u32) -> String {
    fs::read_link(format!("/proc/{}/exe", pid))
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn current_euid() -> u32 {
    unsafe { libc::geteuid() }
}

fn get_process_uid(pid: u32) -> Option<u32> {
    if pid == 0 {
        return None;
    }

    fs::read_to_string(format!("/proc/{}/status", pid))
        .ok()?
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u32>().ok())
}

fn is_known_system_process(name: &str, cmdline: &str, exe: &str) -> bool {
    let hay = format!("{name} {cmdline} {exe}").to_lowercase();
    const MARKERS: &[&str] = &[
        "ulauncher",
        "gnome-shell",
        "gnome-session",
        "gsd-",
        "xdg-desktop",
        "xdg-document",
        "xdg-permission",
        "pipewire",
        "wireplumber",
        "pulseaudio",
        "systemd",
        "dbus-daemon",
        "cupsd",
        "avahi",
        "networkmanager",
        "firewalld",
        "snapd",
        "packagekit",
        "polkit",
        "gdm",
        "lightdm",
        "sddm",
        "fcitx",
        "ibus-daemon",
        "evolution-",
        "tracker-",
        "gvfsd",
        "flatpak",
        "docker-proxy",
        "containerd",
        "ssh-agent",
        "gpg-agent",
        "kwalletd",
        "plasmashell",
        "kwin_",
    ];

    MARKERS.iter().any(|marker| hay.contains(marker))
}

fn compute_is_protected(pid: u32, uid: u32, euid: u32, is_system: bool) -> bool {
    pid == 0 || is_system || uid != euid
}

fn is_process_protected(pid: u32, uid: u32, name: &str, cmdline: &str) -> bool {
    let is_system = pid != 0 && is_known_system_process(name, cmdline, &get_process_exe(pid));
    compute_is_protected(pid, uid, current_euid(), is_system)
}

fn is_pid_protected(pid: u32) -> bool {
    let uid = get_process_uid(pid).unwrap_or_else(current_euid);
    let cmdline = get_cmdline(pid);
    let name = resolve_display_name(pid, "", &cmdline);
    is_process_protected(pid, uid, &name, &cmdline)
}

fn make_port_entry(
    port: u16,
    pid: u32,
    pids: Vec<u32>,
    process_name: String,
    cmdline: String,
    cwd: String,
    cpu_percent: f32,
    mem_mb: f32,
    uptime_seconds: u64,
) -> PortEntry {
    let uid = get_process_uid(pid).unwrap_or_else(current_euid);
    let is_protected = is_process_protected(pid, uid, &process_name, &cmdline);

    PortEntry {
        port,
        pid,
        pids,
        process_name,
        cmdline,
        cwd,
        state: "listening".to_string(),
        cpu_percent,
        mem_mb,
        uptime_seconds,
        uid,
        is_protected,
    }
}

fn send_signal(pid: u32, signal: i32) -> Result<(), String> {
    if pid == 0 {
        return Err("Invalid PID".to_string());
    }

    let result = unsafe { libc::kill(pid as i32, signal) };
    if result == 0 {
        return Ok(());
    }

    let errno = unsafe { *libc::__errno_location() };
    if errno == libc::ESRCH {
        return Ok(());
    }

    Err(format!(
        "Failed to send signal {} to process {} (errno {})",
        signal, pid, errno
    ))
}

fn get_child_pids(pid: u32) -> Vec<u32> {
    let children_path = format!("/proc/{}/task/{}/children", pid, pid);
    let Ok(content) = fs::read_to_string(children_path) else {
        return Vec::new();
    };

    content
        .split_whitespace()
        .filter_map(|value| value.parse::<u32>().ok())
        .collect()
}

fn kill_process_tree(pid: u32, signal: i32) -> Result<(), String> {
    if pid == 0 {
        return Err("Invalid PID".to_string());
    }

    let mut stack = vec![pid];
    let mut ordered = Vec::new();
    let mut seen = std::collections::HashSet::new();

    while let Some(current) = stack.pop() {
        if !seen.insert(current) {
            continue;
        }
        ordered.push(current);
        for child in get_child_pids(current) {
            stack.push(child);
        }
    }

    for current in ordered.iter().rev().copied() {
        let _ = send_signal(current, signal);
    }

    send_signal(pid, signal)
}

fn num_cpus() -> f32 {
    static NUM_CPUS: OnceLock<f32> = OnceLock::new();
    *NUM_CPUS.get_or_init(|| {
        fs::read_to_string("/proc/cpuinfo")
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("processor"))
            .count()
            .max(1) as f32
    })
}

fn parse_proc_stat(stat: &str) -> Option<(u64, u64, u64)> {
    // Field 3 (state) follows the final ')' of field 2 (comm).
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace();
    let utime = fields.nth(14 - 3)?.parse().ok()?;
    let stime = fields.next()?.parse().ok()?;
    let starttime = fields.nth(22 - 16)?.parse().ok()?;
    Some((utime, stime, starttime))
}

type ProcessKey = (u32, u64);
type CpuSamples = HashMap<ProcessKey, (u64, Instant)>;

struct CpuScan {
    // Keep sample updates and pruning together if another scan is already running.
    samples: MutexGuard<'static, CpuSamples>,
    readings: HashMap<ProcessKey, f32>,
}

impl CpuScan {
    fn new() -> Self {
        static CPU_SAMPLES: OnceLock<Mutex<CpuSamples>> = OnceLock::new();
        Self {
            samples: CPU_SAMPLES
                .get_or_init(Mutex::default)
                .lock()
                .unwrap_or_else(|err| err.into_inner()),
            readings: HashMap::new(),
        }
    }

    fn finish(mut self) {
        self.samples.retain(|key, _| self.readings.contains_key(key));
    }
}

fn cpu_usage_percent(delta_ticks: u64, elapsed_secs: f64, clk_tck: f64, cpus: f32) -> f32 {
    if elapsed_secs <= 0.0 {
        return 0.0;
    }
    (delta_ticks as f64 / clk_tck / elapsed_secs * 100.0).clamp(0.0, 100.0 * f64::from(cpus))
        as f32
}

fn get_process_stats(pid: u32, cpu_scan: &mut CpuScan) -> (f32, f32, u64) {
    let mem_mb = fs::read_to_string(format!("/proc/{}/status", pid))
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f32>().ok())
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0);

    let (cpu_percent, uptime_secs) = (|| -> Option<(f32, u64)> {
        let stat = fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
        let now = Instant::now();
        let (utime, stime, starttime) = parse_proc_stat(&stat)?;
        let uptime_str = fs::read_to_string("/proc/uptime").ok()?;
        let uptime: f64 = uptime_str.split_whitespace().next()?.parse().ok()?;
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let clk_tck = if clk_tck > 0 { clk_tck as f64 } else { 100.0 };
        let seconds = (uptime - starttime as f64 / clk_tck).max(0.0);
        let total_ticks = utime + stime;
        let key = (pid, starttime);
        // A process listening on multiple sockets gets one CPU reading per scan.
        let cpu = *cpu_scan.readings.entry(key).or_insert_with(|| {
            let previous_sample = cpu_scan.samples.insert(key, (total_ticks, now));
            let (delta_ticks, elapsed_secs) = match previous_sample {
                Some((previous_ticks, previous_time)) => (
                    total_ticks.saturating_sub(previous_ticks),
                    now.duration_since(previous_time).as_secs_f64(),
                ),
                None => (total_ticks, seconds),
            };
            cpu_usage_percent(delta_ticks, elapsed_secs, clk_tck, num_cpus())
        });
        Some((cpu, seconds as u64))
    })()
    .unwrap_or((0.0, 0));

    (cpu_percent, mem_mb, uptime_secs)
}

fn merge_entries_by_port(entries: Vec<PortEntry>) -> Vec<PortEntry> {
    let mut by_port = BTreeMap::new();
    for mut entry in entries {
        let pids = std::mem::take(&mut entry.pids);
        let is_protected = entry.is_protected;
        let merged = by_port.entry(entry.port).or_insert(entry);
        merged.is_protected |= is_protected;
        for pid in pids {
            if !merged.pids.contains(&pid) {
                merged.pids.push(pid);
            }
        }
    }
    by_port.into_values().collect()
}

// ─── Port scanning via `ss` (fast) ───────────────────────────────────────────
//
// `ss -tlnp` output (no header):
//   LISTEN 0 128 0.0.0.0:3000 0.0.0.0:* users:(("node",pid=1234,fd=22))
//   LISTEN 0 128 [::]:8080    [::]:*     users:(("python3",pid=5678,fd=5))

fn scan_with_ss(show_all: bool) -> Option<Vec<PortEntry>> {
    let output = std::process::Command::new("ss")
        .args(["-tlnp", "--no-header"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();
    let mut cpu_scan = CpuScan::new();

    for line in stdout.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Fields: State RecvQ SendQ LocalAddr:Port PeerAddr:Port [Process...]
        // Process names can contain spaces (e.g. next-server (v16.2.7)), so the
        // users:(...) blob may span multiple whitespace-separated fields.
        if fields.len() < 5 {
            continue;
        }

        let local = fields[3]; // e.g. "0.0.0.0:3000" or "[::]:8080" or "*:18789"
        let port_str = local.rsplit(':').next().unwrap_or("");
        let Ok(port) = port_str.parse::<u16>() else {
            continue;
        };

        // Prefer the full users:(...) span from the raw line — not fields[5] alone.
        let process_info = line
            .find("users:(")
            .map(|idx| &line[idx..])
            .unwrap_or("");
        let (pid, ss_name) = parse_ss_process(process_info);
        let mut pids = parse_ss_pids(process_info);
        if pids.is_empty() && pid > 0 {
            pids.push(pid);
        }

        // Only show ports that have an associated process we can identify, 
        // to filter out system-level ports (like 53 for DNS) that we can't manage.
        if !show_all && pid == 0 && ss_name.is_empty() {
            continue;
        }

        let cmdline = if pid > 0 { get_cmdline(pid) } else { String::new() };
        let cwd = if pid > 0 { get_process_cwd(pid) } else { String::new() };
        let (cpu_percent, mem_mb, uptime_seconds) = if pid > 0 {
            get_process_stats(pid, &mut cpu_scan)
        } else {
            (0.0, 0.0, 0)
        };
        let process_name = resolve_display_name(pid, &ss_name, &cmdline);

        entries.push(make_port_entry(
            port,
            pid,
            pids,
            process_name,
            cmdline,
            cwd,
            cpu_percent,
            mem_mb,
            uptime_seconds,
        ));
    }

    cpu_scan.finish();
    Some(merge_entries_by_port(entries))
}

/// Parse `users:(("node",pid=1234,fd=5))` → (1234, "node")
fn parse_ss_process(users: &str) -> (u32, String) {
    // Find pid=NNN
    let pid = users
        .split("pid=")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.split(')').next())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    // Find process name between first `("` and `"`
    let name = users
        .split("((\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap_or("")
        .to_string();

    (pid, name)
}

fn parse_ss_pids(users: &str) -> Vec<u32> {
    users
        .split("pid=")
        .skip(1)
        .filter_map(|chunk| {
            let digits: String = chunk.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            digits.parse::<u32>().ok()
        })
        .collect()
}

// ─── Fallback: /proc/net/tcp parsing ─────────────────────────────────────────

fn scan_with_proc(show_all: bool) -> Vec<PortEntry> {
    use std::path::Path;

    // Build inode → pid map
    let mut inode_to_pid: HashMap<u64, u32> = HashMap::new();
    if let Ok(procs) = fs::read_dir("/proc") {
        for entry in procs.flatten() {
            let name = entry.file_name();
            let Some(pid_str) = name.to_str() else { continue };
            let Ok(pid) = pid_str.parse::<u32>() else { continue };
            let fd_dir = Path::new("/proc").join(pid_str).join("fd");
            let Ok(fds) = fs::read_dir(&fd_dir) else { continue };
            for fd in fds.flatten() {
                if let Ok(target) = fs::read_link(fd.path()) {
                    let s = target.to_string_lossy();
                    if s.starts_with("socket:[") && s.ends_with(']') {
                        if let Ok(inode) = s[8..s.len() - 1].parse::<u64>() {
                            inode_to_pid.insert(inode, pid);
                        }
                    }
                }
            }
        }
    }

    let mut cpu_scan = CpuScan::new();
    let mut all = parse_proc_tcp("/proc/net/tcp", &inode_to_pid, show_all, &mut cpu_scan);
    all.extend(parse_proc_tcp("/proc/net/tcp6", &inode_to_pid, show_all, &mut cpu_scan));

    cpu_scan.finish();
    merge_entries_by_port(all)
}

fn parse_proc_tcp(
    path: &str,
    inode_to_pid: &HashMap<u64, u32>,
    show_all: bool,
    cpu_scan: &mut CpuScan,
) -> Vec<PortEntry> {
    let Ok(content) = fs::read_to_string(path) else { return vec![] };
    let mut results = Vec::new();

    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 10 || fields[3] != "0A" { continue; }

        let inode: u64 = fields[9].parse().unwrap_or(0);
        if inode == 0 { continue; }

        // Decode port from local_address hex (last 4 hex chars after ':')
        let local = fields[1];
        let port_hex = local.rsplit(':').next().unwrap_or("");
        let Ok(port) = u16::from_str_radix(port_hex, 16) else { continue };

        let pid = inode_to_pid.get(&inode).copied().unwrap_or(0);
        let pids = if pid > 0 { vec![pid] } else { vec![] };
        let cmdline = if pid > 0 { get_cmdline(pid) } else { String::new() };
        let process_name = resolve_display_name(pid, "", &cmdline);

        if !show_all && pid == 0 {
            continue;
        }
        let cwd = if pid > 0 { get_process_cwd(pid) } else { String::new() };
        let (cpu_percent, mem_mb, uptime_seconds) = if pid > 0 {
            get_process_stats(pid, cpu_scan)
        } else {
            (0.0, 0.0, 0)
        };

        results.push(make_port_entry(
            port,
            pid,
            pids,
            process_name,
            cmdline,
            cwd,
            cpu_percent,
            mem_mb,
            uptime_seconds,
        ));
    }
    results
}

// ─── Tauri Commands ───────────────────────────────────────────────────────────

#[tauri::command]
async fn scan_ports(show_all: bool) -> Result<Vec<PortEntry>, String> {
    append_log(&format!(">>> scan_ports called with show_all={show_all}"));

    let (tx, rx) = mpsc::channel();
    tauri::async_runtime::spawn_blocking(move || {
        let res = if let Some(entries) = scan_with_ss(show_all) {
            append_log(&format!(
                ">>> scan_with_ss succeeded with {} entries",
                entries.len()
            ));
            entries
        } else {
            append_log(">>> scan_with_ss failed, falling back to scan_with_proc");
            let entries = scan_with_proc(show_all);
            append_log(&format!(
                ">>> scan_with_proc succeeded with {} entries",
                entries.len()
            ));
            entries
        };

        append_log(">>> scan_ports returning to JS");
        let _ = tx.send(res);
    });

    tauri::async_runtime::spawn_blocking(move || match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(entries) => Ok(entries),
        Err(_) => {
            append_log(">>> scan_ports timed out");
            Err("scan timed out after 10 seconds".to_string())
        }
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
fn kill_processes(pids: Vec<u32>, force: bool) -> Result<(), String> {
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    append_log(&format!(
        ">>> kill_processes called with signal={} pids={:?}",
        signal, pids
    ));

    let killable: Vec<u32> = pids.into_iter().filter(|pid| *pid > 0).collect();
    if killable.is_empty() {
        return Err("No valid process IDs to kill".to_string());
    }

    let mut errors = Vec::new();

    for pid in killable {
        if is_pid_protected(pid) {
            let err = format!(
                "Process {} is protected (owned by another user or system)",
                pid
            );
            append_log(&format!(">>> kill_processes blocked pid={}", pid));
            errors.push(err);
            continue;
        }

        if let Err(err) = kill_process_tree(pid, signal) {
            append_log(&format!(">>> kill_processes error pid={} err={}", pid, err));
            errors.push(err);
        } else {
            append_log(&format!(">>> kill_processes signaled pid={}", pid));
        }
    }

    if errors.is_empty() {
        append_log(">>> kill_processes completed successfully");
        Ok(())
    } else {
        append_log(&format!(
            ">>> kill_processes completed with {} error(s)",
            errors.len()
        ));
        Err(errors.join("; "))
    }
}

#[tauri::command]
fn kill_process(pid: u32) -> Result<(), String> {
    kill_processes(vec![pid], false)
}

#[tauri::command]
fn force_kill_process(pid: u32) -> Result<(), String> {
    kill_processes(vec![pid], true)
}

#[tauri::command]
fn open_in_browser(url: String) -> Result<(), String> {
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

// ─── App Entry ────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let quit = MenuItemBuilder::new("Quit harbor").id("quit").build(app)?;
            let show = MenuItemBuilder::new("Show").id("show").build(app)?;
            let menu = MenuBuilder::new(app)
                .item(&show)
                .separator()
                .item(&quit)
                .build()?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "quit" => app.exit(0),
                    "show" => toggle_window(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        position,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        show_or_hide_near_tray(app, position.x, position.y);
                    }
                })
                .build(app)?;

            // Start hidden — only show on tray click
            if let Some(win) = app.get_webview_window("main") {
                win.hide().ok();
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            scan_ports,
            kill_processes,
            kill_process,
            force_kill_process,
            open_in_browser,
            quit_app
        ])
        .run(tauri::generate_context!())
        .expect("error while running harbor");
}

// ─── Window helpers ───────────────────────────────────────────────────────────

fn toggle_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        if win.is_visible().unwrap_or(false) {
            win.hide().ok();
        } else {
            win.show().ok();
            win.set_focus().ok();
        }
    }
}

fn show_or_hide_near_tray(app: &AppHandle, tray_x: f64, tray_y: f64) {
    let Some(win) = app.get_webview_window("main") else { return };

    if win.is_visible().unwrap_or(false) {
        win.hide().ok();
        return;
    }

    let win_w = 400.0f64;
    let win_h = 520.0f64;
    let margin = 12.0f64;

    // Get screen dimensions to anchor to the correct edge
    let (x, y) = win
        .primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| {
            let screen_w = monitor.size().width as f64;
            let screen_h = monitor.size().height as f64;
            let mon_x = monitor.position().x as f64;
            let mon_y = monitor.position().y as f64;

            // Horizontal: clamp so window stays on screen, biased to right edge
            let raw_x = tray_x - win_w / 2.0;
            let x = raw_x
                .max(mon_x + margin)
                .min(mon_x + screen_w - win_w - margin);

            // Vertical: if tray is in bottom half → show above; top half → show below
            let y = if tray_y > mon_y + screen_h / 2.0 {
                // bottom tray (e.g. GNOME bottom panel)
                tray_y - win_h - margin
            } else {
                // top tray (e.g. GNOME top bar)
                tray_y + margin
            };
            let y = y.max(mon_y + margin).min(mon_y + screen_h - win_h - margin);

            (x, y)
        })
        .unwrap_or_else(|| {
            // Fallback if monitor info unavailable
            let x = (tray_x - win_w / 2.0).max(0.0);
            let y = (tray_y - win_h - margin).max(0.0);
            (x, y)
        });

    win.set_position(tauri::PhysicalPosition::new(x as i32, y as i32))
        .ok();
    win.show().ok();
    win.set_focus().ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn parses_proc_stat_after_the_process_name() {
        let fields = "S 4 5 6 7 8 9 10 11 12 13 140 150 16 17 18 19 20 21 220 23";
        for name in ["node", "next-server (v16.2.7)", "worker) pool"] {
            let stat = format!("123 ({name}) {fields}");
            assert_eq!(parse_proc_stat(&stat), Some((140, 150, 220)));
        }
    }

    #[test]
    fn rejects_incomplete_proc_stat() {
        assert_eq!(parse_proc_stat("123 missing-comm S 4 5"), None);
        assert_eq!(parse_proc_stat("123 (node) S 4 5"), None);
    }

    #[test]
    fn calculates_cpu_usage_from_elapsed_time_and_ticks() {
        assert_eq!(cpu_usage_percent(50, 2.0, 100.0, 4.0), 25.0);
        assert_eq!(cpu_usage_percent(50, 0.0, 100.0, 4.0), 0.0);
        assert_eq!(cpu_usage_percent(50, -1.0, 100.0, 4.0), 0.0);
        assert_eq!(cpu_usage_percent(0, 1.0, 100.0, 4.0), 0.0);
        assert_eq!(cpu_usage_percent(1000, 1.0, 100.0, 4.0), 400.0);
    }

    #[test]
    fn protects_zero_pid_system_processes_and_other_users() {
        assert!(compute_is_protected(0, 1000, 1000, false));
        assert!(compute_is_protected(42, 1000, 1000, true));
        assert!(compute_is_protected(42, 1001, 1000, false));
        assert!(!compute_is_protected(42, 1000, 1000, false));
    }

    #[test]
    fn merges_ports_using_first_entry_and_unique_pids() {
        let first = PortEntry {
            port: 8080,
            pid: 42,
            pids: vec![42, 42, 43],
            process_name: "first".to_string(),
            cmdline: String::new(),
            cwd: String::new(),
            state: "listening".to_string(),
            cpu_percent: 0.0,
            mem_mb: 0.0,
            uptime_seconds: 0,
            uid: 1000,
            is_protected: false,
        };
        let second = PortEntry {
            pid: 44,
            pids: vec![44, 43, 44],
            process_name: "second".to_string(),
            is_protected: true,
            ..first.clone()
        };
        let other_port = PortEntry {
            port: 3000,
            ..first.clone()
        };

        let merged = merge_entries_by_port(vec![first, other_port, second]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].port, 3000);
        assert_eq!(merged[0].pids, vec![42, 43]);
        assert_eq!(merged[1].port, 8080);
        assert_eq!(merged[1].pid, 42);
        assert_eq!(merged[1].process_name, "first");
        assert!(merged[1].is_protected);
        assert_eq!(merged[1].pids, vec![42, 43, 44]);
    }

    fn pid_alive(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }

        // Zombies still exist in /proc and accept kill(0), but they are already dead.
        if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid)) {
            if let Some(end) = stat.rfind(')') {
                let state = stat[end + 1..].trim().chars().next();
                if state == Some('Z') {
                    return false;
                }
            }
        }

        let result = unsafe { libc::kill(pid as i32, 0) };
        if result == 0 {
            return true;
        }

        let errno = unsafe { *libc::__errno_location() };
        errno != libc::ESRCH
    }

    fn wait_until_dead(pid: u32, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !pid_alive(pid) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!pid_alive(pid), "pid {pid} still alive after timeout");
    }

    #[test]
    fn kill_processes_terminates_a_simple_process() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");

        let pid = child.id();
        assert!(pid_alive(pid));

        kill_processes(vec![pid], true).expect("kill process");
        wait_until_dead(pid, Duration::from_secs(3));

        let _ = child.wait();
    }

    #[test]
    fn kill_processes_terminates_a_process_tree() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30 & wait")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn shell");

        let pid = child.id();

        let children_path = format!("/proc/{}/task/{}/children", pid, pid);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut spawned_child = 0;
        while Instant::now() < deadline {
            if let Ok(content) = fs::read_to_string(&children_path) {
                spawned_child = content
                    .split_whitespace()
                    .find_map(|value| value.parse::<u32>().ok())
                    .unwrap_or(0);
                if spawned_child > 0 {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }

        assert!(spawned_child > 0, "expected shell child process to exist");
        assert!(pid_alive(pid));
        assert!(pid_alive(spawned_child));

        kill_processes(vec![pid], true).expect("kill process tree");
        wait_until_dead(pid, Duration::from_secs(3));
        wait_until_dead(spawned_child, Duration::from_secs(3));

        let _ = child.wait();
    }

    #[test]
    #[ignore = "requires live localhost ports (same-user next on :3000, ulauncher on :5054)"]
    fn scan_marks_next_killable_and_ulauncher_protected() {
        let Some(entries) = scan_with_ss(true) else {
            return;
        };

        if let Some(next) = entries.iter().find(|e| e.port == 3000) {
            eprintln!(
                "next entry: name={} cmdline={} cwd={} uid={} protected={} pid={}",
                next.process_name, next.cmdline, next.cwd, next.uid, next.is_protected, next.pid
            );
            assert!(
                !next.is_protected,
                "next on :3000 should be killable, got name={} cmdline={} cwd={} uid={}",
                next.process_name,
                next.cmdline,
                next.cwd,
                next.uid
            );
            assert!(
                next.process_name.to_lowercase().contains("next"),
                "expected next-like name, got {}",
                next.process_name
            );
        }

        if let Some(ulauncher) = entries.iter().find(|e| e.port == 5054) {
            assert!(
                ulauncher.is_protected,
                "ulauncher on :5054 should be protected"
            );
        }
    }
}
