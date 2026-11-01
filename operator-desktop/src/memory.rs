// SPDX-License-Identifier: MIT
//! On-demand accounting of the selected local Runtime and its native children.
//! Remote Docker accounting uses the fixed host transport instead.
use serde::Serialize;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize)]
pub struct MemorySnapshot {
    pub available: bool,
    pub partial: bool,
    pub sampled_at_unix_ms: u64,
    /// Number of processes whose RSS was included in this sample.
    pub process_count: usize,
    pub rss_bytes: Option<u64>,
    /// Present only when all discovered processes have readable PSS and the
    /// process-tree walk completed within its limits.
    pub pss_bytes: Option<u64>,
    pub scope: &'static str,
    pub rss_shared_pages_counted_multiple_times: bool,
    pub error: Option<MemoryError>,
}

#[derive(Debug, Serialize)]
pub struct MemoryError {
    code: &'static str,
    message: &'static str,
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
impl MemorySnapshot {
    fn unavailable(code: &'static str, message: &'static str) -> Self {
        Self {
            available: false,
            partial: false,
            sampled_at_unix_ms: timestamp(),
            process_count: 0,
            rss_bytes: None,
            pss_bytes: None,
            scope: "local_runtime_process_tree",
            rss_shared_pages_counted_multiple_times: true,
            error: Some(MemoryError { code, message }),
        }
    }
}

pub fn sample_runtime(options: &runtime_operator::Options) -> Result<Value, String> {
    let snapshot =
        runtime_operator::console::request(options, runtime_control::console::Command::Snapshot)?;
    let stopped = || json!({"available":true,"running":false,"scope":"local_runtime_process_tree","current_bytes":0,"peak_bytes":null,"limit_bytes":null,"sampled_at_unix_ms":timestamp()});
    if snapshot["worker_online"] != true {
        return Ok(stopped());
    }
    let pid = snapshot["host"]["pid"]
        .as_u64()
        .and_then(|p| u32::try_from(p).ok())
        .filter(|p| *p > 1)
        .ok_or("runtime_pid_unavailable")?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let args =
            std::fs::read(format!("/proc/{pid}/cmdline")).map_err(|_| "runtime_pid_unavailable")?;
        if args.len() > 65536 {
            return Err("runtime_pid_invalid".into());
        }
        let words: Vec<_> = args.split(|b| *b == 0).collect();
        if !words
            .windows(2)
            .any(|w| w[0] == b"--state" && w[1] == options.state.as_os_str().as_bytes())
            || !words.windows(2).any(|w| w[0] == b"agent" && w[1] == b"run")
        {
            return Err("runtime_pid_mismatch".into());
        }
        let before =
            std::fs::read(format!("/proc/{pid}/stat")).map_err(|_| "runtime_pid_unavailable")?;
        let sample = linux::sample(pid);
        let after =
            std::fs::read(format!("/proc/{pid}/stat")).map_err(|_| "runtime_pid_unavailable")?;
        // The start time follows the final ')' even when a process name has spaces.
        let start = |b: &[u8]| {
            b.iter()
                .rposition(|v| *v == b')')
                .and_then(|i| b.get(i + 2..))
                .and_then(|v| v.split(|v| v.is_ascii_whitespace()).nth(19))
                .map(|v| v.to_vec())
        };
        if start(&before).is_none() || start(&before) != start(&after) {
            return Err("runtime_pid_changed".into());
        }
        let mut value = serde_json::to_value(sample).map_err(|_| "memory_unavailable")?;
        value["running"] = json!(true);
        Ok(value)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Ok(serde_json::to_value(MemorySnapshot::unavailable(
            "unsupported_platform",
            "Runtime 内存采样暂不可用。",
        ))
        .map_err(|_| "memory_unavailable")?)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{MemoryError, MemorySnapshot, timestamp};
    use std::{
        collections::HashSet,
        fs::{self, File},
        io::{self, Read},
        path::Path,
        time::{Duration, Instant},
    };

    const MAX_PROCESSES: usize = 256;
    const MAX_TASKS: usize = 2048;
    const MAX_FILE_BYTES: u64 = 16 * 1024;
    const WALK_BUDGET: Duration = Duration::from_millis(100);
    const SAMPLE_BUDGET: Duration = Duration::from_millis(250);

    /// Reject malformed units, duplicate fields and overflow rather than
    /// silently publishing a wrong byte count. Process names need not be UTF-8.
    fn kib_field(bytes: &[u8], key: &[u8]) -> Option<u64> {
        let mut result = None;
        for line in bytes.split(|byte| *byte == b'\n') {
            let Some(value) = line.strip_prefix(key) else {
                continue;
            };
            if result.is_some() {
                return None;
            }
            let mut fields = value.split(|byte| byte.is_ascii_whitespace());
            let mut next = || fields.find(|field| !field.is_empty());
            let number = next()?;
            if next()? != b"kB" || next().is_some() {
                return None;
            }
            let kib = decimal(number)?;
            result = Some(kib.checked_mul(1024)?);
        }
        result
    }

    fn decimal(bytes: &[u8]) -> Option<u64> {
        if bytes.is_empty() {
            return None;
        }
        bytes.iter().try_fold(0u64, |value, byte| {
            if !byte.is_ascii_digit() {
                return None;
            }
            value.checked_mul(10)?.checked_add(u64::from(*byte - b'0'))
        })
    }

    fn children(bytes: &[u8]) -> Option<Vec<u32>> {
        let mut pids = Vec::new();
        for field in bytes.split(|byte| byte.is_ascii_whitespace()) {
            if field.is_empty() {
                continue;
            }
            if pids.len() == MAX_PROCESSES {
                return None;
            }
            let pid = u32::try_from(decimal(field)?).ok()?;
            if pid == 0 {
                return None;
            }
            pids.push(pid);
        }
        Some(pids)
    }

    fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        Ok(bytes)
    }

    struct Tree {
        pids: Vec<u32>,
        partial: bool,
    }

    fn process_tree(root: u32, deadline: Instant) -> Tree {
        let mut tree = Tree {
            pids: vec![root],
            partial: false,
        };
        let mut known = HashSet::from([root]);
        let mut cursor = 0;
        let mut tasks = 0;
        while cursor < tree.pids.len() {
            let pid = tree.pids[cursor];
            cursor += 1;
            let Ok(entries) = fs::read_dir(format!("/proc/{pid}/task")) else {
                tree.partial = true;
                continue;
            };
            // A process forked by a background thread is absent from the
            // leader's children file. Inspect each task, with a global bound.
            for entry in entries {
                if tasks >= MAX_TASKS || Instant::now() >= deadline {
                    tree.partial = true;
                    return tree;
                }
                tasks += 1;
                let Some(entry) = entry.ok() else {
                    tree.partial = true;
                    continue;
                };
                let Some(tid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<u32>().ok())
                else {
                    tree.partial = true;
                    continue;
                };
                let Some(children) =
                    read_bounded(Path::new(&format!("/proc/{pid}/task/{tid}/children")))
                        .ok()
                        .and_then(|bytes| children(&bytes))
                else {
                    // Processes and threads may finish between these reads.
                    tree.partial = true;
                    continue;
                };
                for child in children {
                    if known.contains(&child) {
                        continue;
                    }
                    if tree.pids.len() >= MAX_PROCESSES {
                        tree.partial = true;
                        return tree;
                    }
                    known.insert(child);
                    tree.pids.push(child);
                }
            }
        }
        tree
    }

    pub(super) fn sample(root: u32) -> MemorySnapshot {
        let start = Instant::now();
        let sampled_at_unix_ms = timestamp();
        let tree = process_tree(root, start + WALK_BUDGET);
        let mut partial = tree.partial;
        let mut count = 0;
        let mut rss = 0u64;
        let mut pss = (!partial).then_some(0u64);
        for (index, pid) in tree.pids.into_iter().enumerate() {
            // Always attempt the root RSS even if a large tree exhausted the
            // traversal budget; subsequent work stays deadline-bounded.
            if index > 0 && Instant::now() >= start + SAMPLE_BUDGET {
                partial = true;
                break;
            }
            let resident = read_bounded(Path::new(&format!("/proc/{pid}/status")))
                .ok()
                .and_then(|bytes| kib_field(&bytes, b"VmRSS:"));
            let Some(resident) = resident.and_then(|value| rss.checked_add(value)) else {
                partial = true;
                pss = None;
                continue;
            };
            rss = resident;
            count += 1;
            // Once a full PSS total is impossible, avoid further expensive map
            // walks; RSS still covers the remaining readable processes.
            if let Some(sum) = pss {
                pss = read_bounded(Path::new(&format!("/proc/{pid}/smaps_rollup")))
                    .ok()
                    .and_then(|bytes| kib_field(&bytes, b"Pss:"))
                    .and_then(|value| sum.checked_add(value));
            }
            if pss.is_none() {
                partial = true;
            }
        }
        MemorySnapshot {
            available: count > 0,
            partial,
            sampled_at_unix_ms,
            process_count: count,
            rss_bytes: (count > 0).then_some(rss),
            pss_bytes: if partial { None } else { pss },
            scope: "local_runtime_process_tree",
            rss_shared_pages_counted_multiple_times: true,
            error: if count == 0 {
                Some(MemoryError {
                    code: "memory_unavailable",
                    message: "暂时无法读取Runtime的进程内存。",
                })
            } else if partial {
                Some(MemoryError {
                    code: "partial_sample",
                    message: "部分进程、线程或 PSS 不可读；内存采样可能不完整。",
                })
            } else {
                None
            },
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{
            process::{Command, Stdio},
            sync::mpsc,
            thread,
        };

        #[test]
        fn proc_kib_parser_checks_units_duplicates_and_overflow() {
            assert_eq!(
                kib_field(b"Name:\t\xff\nVmRSS:\t1234 kB\n", b"VmRSS:"),
                Some(1_263_616)
            );
            assert_eq!(
                kib_field(b"Pss_Anon: 900 kB\nPss: 0 kB\n", b"Pss:"),
                Some(0)
            );
            for invalid in [
                "VmRSS: 1 MB\n",
                "VmRSS: -1 kB\n",
                "VmRSS: 1 kB trailing\n",
                "VmRSS: 1 kB\nVmRSS: 2 kB\n",
                "VmRSS: 18446744073709551615 kB\n",
                "VmRSS: 18446744073709551616 kB\n",
                "VmRSS: kB\n",
                "VmRSSExtra: 1 kB\n",
            ] {
                assert_eq!(kib_field(invalid.as_bytes(), b"VmRSS:"), None, "{invalid}");
            }
        }

        #[test]
        fn children_parser_rejects_invalid_ids_and_has_a_hard_bound() {
            assert_eq!(children(b" 12 34\n56\t"), Some(vec![12, 34, 56]));
            assert_eq!(children(b"\n"), Some(vec![]));
            for invalid in [b"0".as_slice(), b"4294967296", b"123 x", b"-2"] {
                assert_eq!(children(invalid), None);
            }
            assert!(children("1 ".repeat(MAX_PROCESSES + 1).as_bytes()).is_none());
        }

        struct BackgroundChild {
            pid: u32,
            stop: Option<mpsc::Sender<()>>,
            worker: Option<thread::JoinHandle<()>>,
        }
        impl BackgroundChild {
            fn new() -> Self {
                let (pid_tx, pid_rx) = mpsc::sync_channel(1);
                let (stop_tx, stop_rx) = mpsc::channel();
                let worker = thread::spawn(move || {
                    let mut child = Command::new("/bin/sleep")
                        .arg("10")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .unwrap();
                    pid_tx.send(child.id()).unwrap();
                    let _ = stop_rx.recv_timeout(Duration::from_secs(10));
                    let _ = child.kill();
                    let _ = child.wait();
                });
                Self {
                    pid: pid_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                    stop: Some(stop_tx),
                    worker: Some(worker),
                }
            }
        }
        impl Drop for BackgroundChild {
            fn drop(&mut self) {
                self.stop.take();
                if let Some(worker) = self.worker.take() {
                    worker.join().unwrap();
                }
            }
        }

        #[test]
        fn runtime_tree_samples_children_created_by_background_threads() {
            let child = BackgroundChild::new();
            let tree = process_tree(std::process::id(), Instant::now() + Duration::from_secs(1));
            assert!(
                tree.pids.contains(&child.pid),
                "thread-created child was omitted"
            );
            let snapshot = sample(std::process::id());
            assert!(snapshot.available);
            assert!(snapshot.process_count >= 2);
            assert!(snapshot.rss_bytes.unwrap() > 0);
            assert!(snapshot.sampled_at_unix_ms > 0);
            let json = serde_json::to_value(&snapshot).unwrap();
            assert_eq!(json["scope"], "local_runtime_process_tree");
            assert!(json.get("pid").is_none() && json.get("path").is_none());
            assert_eq!(json["rss_shared_pages_counted_multiple_times"], true);
            assert!(json["pss_bytes"].is_null() || json["pss_bytes"].as_u64().unwrap() > 0);
        }
    }
}
