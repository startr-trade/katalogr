//! An anonymised machine profile: no host name, user name, address or path.

use serde::{Deserialize, Serialize};

use crate::katalogr::Katalogr;
use crate::workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineProfile {
    pub os: String,
    pub arch: String,
    /// major.minor only.
    pub kernel: String,
    pub cpu_model: String,
    pub logical_cpus: u32,
    pub mem_gib: u32,
    /// Filesystem type of the workspace from /proc/mounts, or "unknown".
    pub fs: String,
    /// `katalogr --version` output (version + commit).
    pub katalogr_version: String,
    pub harness_version: String,
    /// "proc" when /proc/loadavg is read, else "none".
    pub load_source: String,
}

fn kernel_major_minor(s: &str) -> String {
    let mut parts = s
        .trim()
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty());
    match (parts.next(), parts.next()) {
        (Some(a), Some(b)) => format!("{a}.{b}"),
        (Some(a), None) => a.to_string(),
        _ => "unknown".into(),
    }
}

fn cpu_model(cpuinfo: &str) -> String {
    cpuinfo
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == "model name").then(|| v.split_whitespace().collect::<Vec<_>>().join(" "))
        })
        .unwrap_or_else(|| "unknown".into())
}

fn mem_gib(meminfo: &str) -> u32 {
    meminfo
        .lines()
        .find_map(|l| {
            let rest = l.strip_prefix("MemTotal:")?;
            rest.split_whitespace().next()?.parse::<u64>().ok()
        })
        .map(|kib| ((kib as f64) / (1024.0 * 1024.0)).round() as u32)
        .unwrap_or(0)
}

/// Longest mount point that is a prefix of `path`.
fn fs_type(mounts: &str, path: &str) -> String {
    let mut best: Option<(usize, &str)> = None;
    for l in mounts.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 3 {
            continue;
        }
        let mp = f[1];
        let hit = path == mp
            || (path.starts_with(mp) && (mp == "/" || path[mp.len()..].starts_with('/')));
        if hit && best.is_none_or(|(n, _)| mp.len() >= n) {
            best = Some((mp.len(), f[2]));
        }
    }
    best.map(|(_, t)| t.to_string())
        .unwrap_or_else(|| "unknown".into())
}

impl MachineProfile {
    pub fn detect(ws: &Workspace, k: &Katalogr) -> MachineProfile {
        let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
        MachineProfile {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            kernel: kernel_major_minor(&read("/proc/sys/kernel/osrelease")),
            cpu_model: cpu_model(&read("/proc/cpuinfo")),
            logical_cpus: std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(1),
            mem_gib: mem_gib(&read("/proc/meminfo")),
            fs: fs_type(&read("/proc/mounts"), ws.root().as_str()),
            katalogr_version: k.version().unwrap_or_else(|_| "unknown".into()),
            harness_version: env!("CARGO_PKG_VERSION").into(),
            load_source: if std::path::Path::new("/proc/loadavg").exists() {
                "proc"
            } else {
                "none"
            }
            .into(),
        }
    }
}

#[cfg(test)]
#[test]
fn profile_is_anonymous() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::open(Some(camino::Utf8Path::from_path(tmp.path()).unwrap())).unwrap();
    let k = Katalogr {
        bin: "/nonexistent/katalogr".into(),
        kindex: ws.kindex(),
        project: "x".into(),
    };
    let p = MachineProfile::detect(&ws, &k);
    let json = serde_json::to_string(&p).unwrap();
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    let host = host.trim();
    if !host.is_empty() && host.len() > 3 {
        assert!(!json.contains(host), "host name leaked: {json}");
    }
    for var in ["USER", "LOGNAME"] {
        if let Ok(u) = std::env::var(var)
            && u.len() > 3
        {
            assert!(!json.contains(&u), "{var} leaked: {json}");
        }
    }
    if let Ok(h) = std::env::var("HOME")
        && h.len() > 1
    {
        assert!(!json.contains(&h), "HOME leaked: {json}");
    }
    assert!(!json.contains(ws.root().as_str()));
    assert_eq!(kernel_major_minor("6.8.0-45-generic\n"), "6.8");
    assert_eq!(
        cpu_model("processor : 0\nmodel name\t: Foo   Bar  3000\n"),
        "Foo Bar 3000"
    );
    assert_eq!(mem_gib("MemTotal:       16384000 kB\n"), 16);
    assert_eq!(
        fs_type(
            "/dev/a / ext4 rw 0 0\n/dev/b /home xfs rw 0 0\n",
            "/home/x/ws"
        ),
        "xfs"
    );
    assert_eq!(
        fs_type(
            "/dev/a / ext4 rw 0 0\n/dev/b /home xfs rw 0 0\n",
            "/homey/ws"
        ),
        "ext4"
    );
}
