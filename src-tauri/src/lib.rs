use ksni::TrayMethods;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

mod autostart;
mod gnome;
mod kde_blur;
mod popup;

// ─── Data Types ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PortEntry {
    pub port: u16,
    pub pid: u32,
    pub pids: Vec<u32>,
    pub process_name: String,
    pub launched_from: String,
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
            if *pkg == ".bin" || pkg.starts_with('@') {
                return parts.get(idx + 2).map(|name| (*name).to_string());
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
fn resolve_display_name(pid: u32, cmdline: &str) -> String {
    if let Some(name) = cmdline_display_name(cmdline) {
        return name;
    }

    let comm = if pid > 0 { get_process_name(pid) } else { String::new() };
    if comm.to_lowercase().starts_with("next-server") {
        "next".to_string()
    } else if comm.is_empty() {
        "unknown".to_string()
    } else {
        comm
    }
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

fn parse_process_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
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
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let uid = parse_process_uid(&status).unwrap_or_else(current_euid);
    let cmdline = get_cmdline(pid);
    let name = resolve_display_name(pid, &cmdline);
    is_process_protected(pid, uid, &name, &cmdline)
}

fn make_port_entry(socket: ListeningSocket, pid: u32, cpu_scan: &mut CpuScan) -> PortEntry {
    let (pids, cmdline, cwd, uid, mem_mb, cpu_percent, uptime_seconds, launched_from) = if pid == 0 {
        (vec![], String::new(), String::new(), socket.uid, 0.0, 0.0, 0, "service".into())
    } else {
        let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        let uid = parse_process_uid(&status).unwrap_or(socket.uid);
        let mem_mb = status
            .lines()
            .find(|line| line.starts_with("VmRSS:"))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<f32>().ok())
            .map_or(0.0, |kb| kb / 1024.0);
        let (cpu_percent, uptime_seconds, launched_from) = get_process_stats(pid, cpu_scan);
        (
            vec![pid],
            get_cmdline(pid),
            get_process_cwd(pid),
            uid,
            mem_mb,
            cpu_percent,
            uptime_seconds,
            launched_from,
        )
    };
    let process_name = resolve_display_name(pid, &cmdline);
    let is_protected = is_process_protected(pid, uid, &process_name, &cmdline);

    PortEntry {
        port: socket.port,
        pid,
        pids,
        process_name,
        launched_from,
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

fn split_proc_stat(stat: &str) -> Option<(&str, &str)> {
    stat.split_once('(')?.1.rsplit_once(')')
}

fn parse_proc_stat(stat: &str) -> Option<(u32, u64, u64, u64)> {
    // Field 3 (state) follows the final ')' of field 2 (comm).
    let mut fields = split_proc_stat(stat)?.1.split_whitespace();
    let parent = fields.nth(1)?.parse().ok()?;
    let utime = fields.nth(14 - 5)?.parse().ok()?;
    let stime = fields.next()?.parse().ok()?;
    let starttime = fields.nth(22 - 16)?.parse().ok()?;
    Some((parent, utime, stime, starttime))
}

fn resolve_launch_app(ancestors: impl IntoIterator<Item = String>) -> String {
    let mut fallback = None;
    for ancestor in ancestors {
        let name = ancestor.trim();
        let known = match name.to_ascii_lowercase().as_str() {
            "systemd" | "init" => break,
            "bash" | "dash" | "sh" | "zsh" | "fish" | "ksh" | "tcsh" | "csh" | "nu"
            | "login" | "sudo" | "su" | "doas" | "env" => continue,
            "ptyxis" | "ptyxis-agent" => "Ptyxis",
            "gnome-terminal" | "gnome-terminal-" | "gnome-terminal-server" => "GNOME Terminal",
            "kgx" | "gnome-console" => "GNOME Console",
            "konsole" => "Konsole",
            "kitty" => "Kitty",
            "alacritty" => "Alacritty",
            "wezterm" | "wezterm-gui" => "WezTerm",
            "foot" | "footclient" => "Foot",
            "ghostty" => "Ghostty",
            "tilix" => "Tilix",
            "terminator" => "Terminator",
            "xterm" => "XTerm",
            "code" | "code-insiders" | "code-oss" => "VS Code",
            "codium" | "vscodium" => "VSCodium",
            "cursor" => "Cursor",
            "zed" | "zed-editor" => "Zed",
            "idea" | "idea64" | "intellij-idea" => "IntelliJ IDEA",
            "pycharm" => "PyCharm",
            "webstorm" => "WebStorm",
            "goland" => "GoLand",
            "rustrover" => "RustRover",
            "clion" => "CLion",
            "phpstorm" => "PhpStorm",
            "rubymine" => "RubyMine",
            "datagrip" => "DataGrip",
            "rider" => "Rider",
            "jetbrains" => "JetBrains",
            "tmux" | "tmux: server" | "tmux: client" => "tmux",
            "zellij" => "Zellij",
            "herdr" => "Herdr",
            _ => {
                if fallback.is_none() && !name.is_empty() {
                    fallback = Some(name.to_owned());
                }
                continue;
            }
        };
        return known.into();
    }
    fallback.unwrap_or_else(|| "service".into())
}

fn read_launch_app(mut parent: u32) -> String {
    resolve_launch_app(std::iter::from_fn(|| {
        if parent == 0 {
            return None;
        }
        let stat = fs::read_to_string(format!("/proc/{parent}/stat")).ok()?;
        let (comm, fields) = split_proc_stat(&stat)?;
        let name = if matches!(comm, "java" | "javaw") {
            let cmdline = get_cmdline(parent);
            cmdline
                .split_whitespace()
                .find_map(|arg| arg.strip_prefix("-Didea.platform.prefix="))
                .unwrap_or(comm)
                .to_owned()
        } else {
            comm.to_owned()
        };
        parent = fields.split_whitespace().nth(1)?.parse().ok()?;
        Some(name)
    }).take(64))
}

type ProcessKey = (u32, u64);
type CpuSamples = HashMap<ProcessKey, CpuSample>;

struct CpuSample {
    ticks: u64,
    time: Instant,
    launched_from: String,
}

struct CpuScan {
    // Keep sample updates and pruning together if another scan is already running.
    samples: MutexGuard<'static, CpuSamples>,
    readings: HashMap<ProcessKey, f32>,
    uptime: Option<f64>,
    clk_tck: f64,
}

impl CpuScan {
    fn new() -> Self {
        static CPU_SAMPLES: OnceLock<Mutex<CpuSamples>> = OnceLock::new();
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let uptime = fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|text| text.split_whitespace().next()?.parse().ok());
        Self {
            samples: CPU_SAMPLES
                .get_or_init(Mutex::default)
                .lock()
                .unwrap_or_else(|err| err.into_inner()),
            readings: HashMap::new(),
            uptime,
            clk_tck: if clk_tck > 0 { clk_tck as f64 } else { 100.0 },
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

fn get_process_stats(pid: u32, cpu_scan: &mut CpuScan) -> (f32, u64, String) {
    let Some((parent, utime, stime, starttime)) = fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| parse_proc_stat(&stat))
    else {
        return (0.0, 0, "service".into());
    };
    let now = Instant::now();
    let clk_tck = cpu_scan.clk_tck;
    let seconds = (cpu_scan.uptime.unwrap_or(0.0) - starttime as f64 / clk_tck).max(0.0);
    let total_ticks = utime + stime;
    let key = (pid, starttime);
    let mut elapsed = seconds;
    let mut delta = total_ticks;
    let sample = cpu_scan.samples.entry(key).and_modify(|sample| {
        elapsed = now.duration_since(sample.time).as_secs_f64();
        delta = total_ticks.saturating_sub(sample.ticks);
    }).or_insert_with(|| CpuSample {
        ticks: total_ticks,
        time: now,
        launched_from: read_launch_app(parent),
    });
    // A process listening on multiple sockets gets one CPU reading per scan.
    let cpu = *cpu_scan.readings.entry(key).or_insert_with(|| {
        sample.ticks = total_ticks;
        sample.time = now;
        cpu_usage_percent(delta, elapsed, clk_tck, num_cpus())
    });
    (cpu, seconds as u64, sample.launched_from.clone())
}

fn merge_entries_by_port(entries: Vec<PortEntry>) -> Vec<PortEntry> {
    use std::collections::btree_map::Entry;
    let mut by_port: BTreeMap<u16, PortEntry> = BTreeMap::new();
    for mut entry in entries {
        match by_port.entry(entry.port) {
            Entry::Vacant(slot) => { slot.insert(entry); }
            Entry::Occupied(mut slot) => {
                let merged = slot.get_mut();
                if entry.pid != 0 && (merged.pid == 0 || entry.pid < merged.pid) {
                    std::mem::swap(merged, &mut entry);
                }
                merged.pids.append(&mut entry.pids);
                merged.is_protected |= entry.is_protected;
            }
        }
    }
    by_port.into_values().map(|mut row| {
        row.pids.sort_unstable();
        row.pids.dedup();
        row
    }).collect()
}

// ─── Listening sockets and their process owners ────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ListeningSocket {
    port: u16,
    inode: u64,
    uid: u32,
}

fn parse_proc_tcp_line(line: &str) -> Option<ListeningSocket> {
    let mut fields = line.split_whitespace();
    let port = u16::from_str_radix(fields.nth(1)?.rsplit_once(':')?.1, 16).ok()?;
    if fields.nth(1)? != "0A" {
        return None;
    }
    let uid = fields.nth(3)?.parse().ok()?;
    let inode = fields.nth(1)?.parse().ok()?;
    (inode != 0).then_some(ListeningSocket { port, inode, uid })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SocketOwner {
    pid: u32,
    fd: u32,
}

type InodeOwners = HashMap<u64, Vec<SocketOwner>>;

fn prune_inode_owners(
    cache: &mut InodeOwners,
    listening_inodes: &HashSet<u64>,
    still_owns: impl Fn(u64, SocketOwner) -> bool,
) -> bool {
    cache.retain(|inode, owners| {
        listening_inodes.contains(inode) && owners.iter().all(|&owner| still_owns(*inode, owner))
    });
    listening_inodes.iter().any(|inode| !cache.contains_key(inode))
}

fn socket_inode(target: &std::path::Path) -> Option<u64> {
    target.to_str()?.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok()
}

fn discover_inode_owners(listening_inodes: &HashSet<u64>) -> InodeOwners {
    let mut cache: InodeOwners = listening_inodes.iter().map(|&inode| (inode, Vec::new())).collect();
    if let Ok(procs) = fs::read_dir("/proc") {
        for process in procs.flatten() {
            let Some(pid) = process.file_name().to_str().and_then(|name| name.parse().ok()) else {
                continue;
            };
            let Ok(fds) = fs::read_dir(process.path().join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                let Ok(target) = fs::read_link(fd.path()) else { continue };
                let Some(owners) = socket_inode(&target).and_then(|inode| cache.get_mut(&inode)) else {
                    continue;
                };
                if let Some(fd) = fd.file_name().to_str().and_then(|name| name.parse().ok()) {
                    if !owners.iter().any(|owner| owner.pid == pid) {
                        owners.push(SocketOwner { pid, fd });
                    }
                }
            }
        }
    }
    cache
}

fn scan_with_proc(show_all: bool) -> Vec<PortEntry> {
    static INODE_OWNERS: OnceLock<Mutex<InodeOwners>> = OnceLock::new();
    let mut cache = INODE_OWNERS.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let mut sockets = Vec::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(content) = fs::read_to_string(path) {
            sockets.extend(content.lines().skip(1).filter_map(parse_proc_tcp_line));
        }
    }
    let listening_inodes = sockets.iter().map(|socket| socket.inode).collect();

    // Checking the cached FD also catches closed listeners and PID reuse before a kill.
    if prune_inode_owners(&mut cache, &listening_inodes, |inode, owner| {
        fs::read_link(format!("/proc/{}/fd/{}", owner.pid, owner.fd))
            .ok().and_then(|target| socket_inode(&target)) == Some(inode)
    }) {
        // Cache unreadable sockets too, or system listeners would trigger a full walk every poll.
        *cache = discover_inode_owners(&listening_inodes);
    }

    let mut cpu_scan = CpuScan::new();
    let mut entries = Vec::new();
    for socket in sockets {
        let owners = &cache[&socket.inode];
        if owners.is_empty() && show_all {
            entries.push(make_port_entry(socket, 0, &mut cpu_scan));
        }
        for owner in owners {
            entries.push(make_port_entry(socket, owner.pid, &mut cpu_scan));
        }
    }
    cpu_scan.finish();
    merge_entries_by_port(entries)
}

// ─── Tauri Commands ───────────────────────────────────────────────────────────

#[tauri::command]
async fn scan_ports(show_all: bool) -> Result<Vec<PortEntry>, String> {
    append_log(&format!(">>> scan_ports called with show_all={show_all}"));

    let scan = tauri::async_runtime::spawn_blocking(move || {
        let entries = scan_with_proc(show_all);
        append_log(&format!(">>> scan_with_proc returned {} entries", entries.len()));
        append_log(">>> scan_ports returning to JS");
        *last_scan() = Some((show_all, entries.clone()));
        entries
    });

    match tokio::time::timeout(Duration::from_secs(10), scan).await {
        Ok(result) => result.map_err(|err| err.to_string()),
        Err(_) => {
            append_log(">>> scan_ports timed out");
            Err("scan timed out after 10 seconds".to_string())
        }
    }
}

/// The last scan outlives the WebView, so a recreated popup can draw it at once
/// while its own first scan runs.
fn last_scan() -> MutexGuard<'static, Option<(bool, Vec<PortEntry>)>> {
    static LAST_SCAN: Mutex<Option<(bool, Vec<PortEntry>)>> = Mutex::new(None);
    LAST_SCAN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[tauri::command]
fn cached_ports(show_all: bool) -> Option<Vec<PortEntry>> {
    last_scan()
        .as_ref()
        .filter(|(scanned_all, _)| *scanned_all == show_all)
        .map(|(_, entries)| entries.clone())
}

fn matching_pids(mut requested: Vec<u32>, listening: &[u32]) -> Vec<u32> {
    requested.retain(|pid| *pid > 0 && listening.contains(pid));
    requested
}

#[tauri::command]
async fn kill_processes(port: u16, pids: Vec<u32>, force: bool) -> Result<(), String> {
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    append_log(&format!(
        ">>> kill_processes called with port={port} signal={signal} pids={pids:?}"
    ));

    let entries = scan_ports(true).await?;
    let listening = entries
        .iter()
        .find(|entry| entry.port == port)
        .map_or(&[][..], |entry| entry.pids.as_slice());
    let killable = matching_pids(pids, listening);
    if killable.is_empty() {
        return Err(format!(
            "Process on :{port} changed or exited; refresh and try again"
        ));
    }

    tauri::async_runtime::spawn_blocking(move || signal_processes(killable, signal))
        .await
        .map_err(|err| err.to_string())?
}

fn signal_processes(killable: Vec<u32>, signal: i32) -> Result<(), String> {
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

const WEBKIT_SOFTWARE_RENDERING: &str = "WEBKIT_DISABLE_COMPOSITING_MODE";

#[tauri::command]
fn open_in_browser(url: String) -> Result<(), String> {
    use gtk::{gdk, gio, gio::prelude::AppLaunchContextExt};

    if !is_local_browser_url(&url) {
        return Err("Expected an HTTP loopback URL with a port".into());
    }

    let context = gdk::Display::default().and_then(|display| display.app_launch_context());
    if let Some(context) = &context {
        context.set_timestamp(gtk::current_event_time());
        context.unsetenv(WEBKIT_SOFTWARE_RENDERING);
    }
    gio::AppInfo::launch_default_for_uri(&url, context.as_ref()).map_err(|err| err.to_string())
}

fn is_local_browser_url(url: &str) -> bool {
    url.strip_prefix("http://localhost:")
        .or_else(|| url.strip_prefix("http://127.0.0.1:"))
        .is_some_and(|port| {
            port.bytes().all(|byte| byte.is_ascii_digit())
                && port.parse::<u16>().is_ok_and(|port| port != 0)
        })
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

struct HarborTray {
    app: AppHandle,
    icon: ksni::Icon,
}

impl ksni::Tray for HarborTray {
    const MENU_ON_ACTIVATE: bool = false;

    fn id(&self) -> String {
        "harbor".into()
    }

    fn title(&self) -> String {
        "harbor".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![self.icon.clone()]
    }

    fn activate(&mut self, x: i32, y: i32) {
        append_log(&format!(">>> tray activate x={x} y={y}"));
        popup::request(&self.app, Some((x, y)), ksni::current_xdg_activation_token());
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;

        vec![StandardItem {
            label: "Open harbor".into(),
            activate: Box::new(|tray: &mut Self| {
                popup::request(&tray.app, None, ksni::current_xdg_activation_token());
            }),
            ..Default::default()
        }
        .into()]
    }
}

/// The tray icon, or None while the GNOME extension provides the panel button.
#[derive(Default)]
struct TrayState(std::sync::Mutex<Option<ksni::Handle<HarborTray>>>);

/// Start the tray icon if it isn't running. Failures are logged; the popup stays
/// reachable through single-instance launches.
async fn start_tray(app: AppHandle) {
    let state = app.state::<TrayState>();
    if state.0.lock().unwrap_or_else(|err| err.into_inner()).is_some() {
        return;
    }
    match spawn_tray(app.clone()).await {
        Ok(handle) => {
            *state.0.lock().unwrap_or_else(|err| err.into_inner()) = Some(handle);
            append_log(">>> tray service started via ksni");
        }
        Err(err) => {
            let message = format!(">>> tray startup failed: {err}");
            append_log(&message);
            eprintln!("{message}");
        }
    }
}

async fn stop_tray(app: &AppHandle) {
    let handle = app.state::<TrayState>().0.lock().unwrap_or_else(|err| err.into_inner()).take();
    if let Some(handle) = handle {
        handle.shutdown().await;
        append_log(">>> tray service stopped (GNOME extension active)");
    }
}

async fn spawn_tray(app: AppHandle) -> Result<ksni::Handle<HarborTray>, String> {
    let image = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))
        .map_err(|err| err.to_string())?;
    let icon = ksni::Icon {
        width: image.width() as i32,
        height: image.height() as i32,
        data: image
            .rgba()
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[3], pixel[0], pixel[1], pixel[2]])
            .collect(),
    };
    HarborTray { app, icon }
        .assume_sni_available(true)
        .spawn()
        .await
        .map_err(|err| err.to_string())
}

// ─── App Entry ────────────────────────────────────────────────────────────────

const ACTIVATION_TOKEN_ARG: &str = "--activation-token=";

// shortcut: mirrors the plugin's private D-Bus API; re-check when bumping the pinned version.
fn forward_activation_token(identifier: &str, token: &str) -> bool {
    use gtk::{gio, glib::variant::ToVariant};

    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return false;
    };
    let name = format!("{identifier}.SingleInstance");
    let path = format!("/{}", name.replace('.', "/").replace('-', "_"));
    let args: Vec<_> = std::env::args_os()
        .map(|arg| arg.to_string_lossy().into_owned())
        .chain(std::iter::once(format!("{ACTIVATION_TOKEN_ARG}{token}")))
        .collect();
    let cwd = std::env::current_dir().unwrap_or_default();

    // The Linux single-instance plugin forwards argv but does not forward the launch environment.
    bus.call_sync(
        Some(&name),
        &path,
        "org.SingleInstance.DBus",
        "ExecuteCallback",
        Some(&(args, cwd.to_string_lossy().as_ref()).to_variant()),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        1000,
        gio::Cancellable::NONE,
    )
    .is_ok()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Start WebKit in software mode: switching after view creation can break wheel scrolling.
    std::env::set_var(WEBKIT_SOFTWARE_RENDERING, "1");
    let context = tauri::generate_context!();
    let startup_token = std::env::var("XDG_ACTIVATION_TOKEN")
        .ok()
        .filter(|token| !token.is_empty());
    if startup_token
        .as_deref()
        .is_some_and(|token| forward_activation_token(&context.config().identifier, token))
    {
        return;
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("error while starting harbor's async runtime");
    tauri::async_runtime::set(runtime.handle().clone());

    let placement_mode = popup::choose_placement_mode();
    let mut app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if !args.iter().skip(1).any(|arg| arg == "--hidden") {
                let token = args
                    .iter()
                    .skip(1)
                    .rev()
                    .filter_map(|arg| arg.strip_prefix(ACTIVATION_TOKEN_ARG))
                    .find(|token| !token.is_empty())
                    .map(str::to_owned);
                popup::request(app, None, token);
            }
        }))
        .on_window_event(popup::on_window_event)
        .setup(move |app| {
            popup::setup(app.handle(), placement_mode);

            app.manage(TrayState::default());
            // Serves the extension's D-Bus API and starts the tray icon unless the
            // GNOME extension is active.
            tauri::async_runtime::spawn(gnome::run(app.handle().clone()));

            if !std::env::args_os().skip(1).any(|arg| arg == "--hidden") {
                popup::request(app.handle(), None, startup_token);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            scan_ports,
            cached_ports,
            kill_processes,
            open_in_browser,
            quit_app,
            popup::resize_popup,
            popup::popup_ready,
            gnome::gnome_integration_status,
            gnome::enable_gnome_integration,
            autostart::autostart_enabled,
            autostart::set_autostart
        ])
        .build(context)
        .expect("error while building harbor");
    app.wry_plugin(popup::ReleaseWebContext::default());
    app.run(|app, event| match event {
        tauri::RunEvent::ExitRequested { code: None, api, .. } => api.prevent_exit(),
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Destroyed,
            ..
        } if label == "main" => popup::on_destroyed(app),
        _ => (),
    });
    runtime.shutdown_background();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn browser_urls_require_http_loopback_and_a_valid_port() {
        for url in ["http://localhost:3000", "http://127.0.0.1:65535"] {
            assert!(is_local_browser_url(url), "{url}");
        }
        for url in [
            "http://localhost",
            "http://localhost:0",
            "http://localhost:65536",
            "http://localhost:+3000",
            "http://localhost:3000/path",
            "http://localhost:3000@evil.example",
            "http://localhost.evil.example:3000",
            "https://127.0.0.1:3000",
            "file:///etc/passwd",
        ] {
            assert!(!is_local_browser_url(url), "{url}");
        }
    }

    #[test]
    fn parses_only_listening_tcp_sockets_from_both_address_families() {
        let ipv4 = "0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000 1000 0 321";
        let ipv6 = "1: 00000000000000000000000001000000:2329 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000 1001 0 654";
        assert_eq!(
            parse_proc_tcp_line(ipv4),
            Some(ListeningSocket { port: 8080, inode: 321, uid: 1000 })
        );
        assert_eq!(
            parse_proc_tcp_line(ipv6),
            Some(ListeningSocket { port: 9001, inode: 654, uid: 1001 })
        );
        assert_eq!(parse_proc_tcp_line(&ipv4.replace(" 0A ", " 01 ")), None);
        assert_eq!(parse_proc_tcp_line("0: 0100007F:1F90 00000000:0000 0A"), None);
    }

    #[test]
    fn prunes_closed_and_stale_inodes_and_keeps_cache_hits() {
        let owner = SocketOwner { pid: 42, fd: 3 };
        let stale = SocketOwner { pid: 43, fd: 4 };
        let mut cache = HashMap::from([
            (1, vec![owner]),
            (2, vec![stale]),
            (3, vec![owner]),
            (4, vec![]),
        ]);
        let listening = HashSet::from([1, 4]);
        assert!(!prune_inode_owners(&mut cache, &listening, |_, _| true));
        assert_eq!(cache, HashMap::from([(1, vec![owner]), (4, vec![])]));

        let with_new_listener = HashSet::from([1, 4, 5]);
        assert!(prune_inode_owners(&mut cache, &with_new_listener, |_, _| true));
        assert!(prune_inode_owners(&mut cache, &listening, |_, _| false));
        assert_eq!(cache, HashMap::from([(4, vec![])]));
    }

    #[test]
    fn resolves_scoped_unscoped_and_bin_package_names() {
        for (path, name) in [
            ("/project/node_modules/@openai/codex/vendor/bin/codex", "codex"),
            ("/project/node_modules/openclaw/dist/index.js", "openclaw"),
            ("/project/node_modules/.bin/tauri", "tauri"),
        ] {
            assert_eq!(package_name_from_script_path(path).as_deref(), Some(name));
        }
        assert_eq!(package_name_from_script_path("/project/node_modules/@openai"), None);
    }

    #[test]
    fn parses_proc_stat_after_the_process_name() {
        let fields = "S 4 5 6 7 8 9 10 11 12 13 140 150 16 17 18 19 20 21 220 23";
        for name in ["node", "next-server (v16.2.7)", "worker) pool"] {
            let stat = format!("123 ({name}) {fields}");
            assert_eq!(parse_proc_stat(&stat), Some((4, 140, 150, 220)));
        }
    }

    #[test]
    fn resolves_launchers_before_falling_back_to_the_nearest_non_shell() {
        let resolve = |names: &[&str]| resolve_launch_app(names.iter().map(|name| (*name).to_owned()));
        assert_eq!(resolve(&["bash", "wrapper", "tmux: server", "kitty"]), "tmux");
        assert_eq!(resolve(&["bash", "gnome-terminal-", "systemd"]), "GNOME Terminal");
        assert_eq!(resolve(&["zsh", "wrapper", "other", "init"]), "wrapper");
        assert_eq!(resolve(&["sh", "systemd", "code"]), "service");
        assert_eq!(resolve(&["fish", "RustRover"]), "RustRover");
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
    fn keeps_only_requested_pids_still_listening_on_the_port() {
        let listening = [42, 43];
        assert_eq!(matching_pids(vec![43, 42], &listening), vec![43, 42]);
        assert_eq!(matching_pids(vec![0, 99, 42], &listening), vec![42]);
        assert!(matching_pids(vec![99], &listening).is_empty());
        assert!(matching_pids(vec![42], &[]).is_empty());
    }

    #[test]
    fn merges_ports_using_lowest_known_pid_and_sorted_unique_pids() {
        let first = PortEntry {
            port: 8080,
            pid: 42,
            pids: vec![42, 42, 43],
            process_name: "first".to_string(),
            launched_from: "Ptyxis".into(),
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

        let unknown = PortEntry { pid: 0, pids: vec![], ..first.clone() };
        let merged = merge_entries_by_port(vec![unknown, second, other_port, first]);
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
    fn signal_processes_terminates_a_simple_process() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");

        let pid = child.id();
        assert!(pid_alive(pid));

        signal_processes(vec![pid], libc::SIGKILL).expect("kill process");
        wait_until_dead(pid, Duration::from_secs(3));

        let _ = child.wait();
    }

    #[test]
    fn signal_processes_terminates_a_process_tree() {
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

        signal_processes(vec![pid], libc::SIGKILL).expect("kill process tree");
        wait_until_dead(pid, Duration::from_secs(3));
        wait_until_dead(spawned_child, Duration::from_secs(3));

        let _ = child.wait();
    }

    #[test]
    #[ignore = "requires live localhost ports (same-user next on :3000, ulauncher on :5054)"]
    fn scan_marks_next_killable_and_ulauncher_protected() {
        let entries = scan_with_proc(true);

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
