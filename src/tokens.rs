use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub claude: u64,
    pub claude_output: u64,
    pub codex: u64,
}

#[derive(Default)]
pub struct Tracker {
    offsets: HashMap<PathBuf, u64>,
    claude_messages: HashSet<String>,
    totals: Totals,
    day: (u16, u16, u16),
}

impl Tracker {
    pub fn poll(&mut self, account_dirs: &[PathBuf], now: i64) -> Totals {
        let day = local_day(now);
        if self.day != day {
            self.day = day;
            self.offsets.clear();
            self.claude_messages.clear();
            self.totals = Totals::default();
        }

        let mut files = Vec::new();
        for dir in account_dirs {
            visit_jsonl(&dir.join("projects"), &mut files, 100);
            if files.len() >= 100 {
                break;
            }
        }
        files.sort_by_key(|path| {
            std::cmp::Reverse(path.metadata().and_then(|meta| meta.modified()).ok())
        });
        for path in files.into_iter().take(100) {
            self.read_claude(&path, day);
        }

        for (path, _, _) in crate::providers::recent_codex_files() {
            self.read_codex(&path, day);
        }
        self.totals
    }

    fn read_claude(&mut self, path: &Path, day: (u16, u16, u16)) {
        self.read_lines(path, |tracker, value| {
            let Some(timestamp) = value["timestamp"]
                .as_str()
                .and_then(crate::api::parse_iso8601)
            else {
                return;
            };
            if local_day(timestamp) != day || value["message"]["usage"].is_null() {
                return;
            }
            let id = value["message"]["id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{}:{timestamp}", path.display()));
            if !tracker.claude_messages.insert(id) {
                return;
            }
            let usage = &value["message"]["usage"];
            let output = usage["output_tokens"].as_u64().unwrap_or(0);
            tracker.totals.claude_output = tracker.totals.claude_output.saturating_add(output);
            tracker.totals.claude = tracker.totals.claude.saturating_add(
                output
                    + usage["input_tokens"].as_u64().unwrap_or(0)
                    + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                    + usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
            );
        });
    }

    fn read_codex(&mut self, path: &Path, day: (u16, u16, u16)) {
        self.read_lines(path, |tracker, value| {
            let Some(timestamp) = value["timestamp"]
                .as_str()
                .and_then(crate::api::parse_iso8601)
            else {
                return;
            };
            if local_day(timestamp) != day
                || value["payload"]["type"].as_str() != Some("token_count")
            {
                return;
            }
            let total = value["payload"]["info"]["last_token_usage"]["total_tokens"]
                .as_u64()
                .unwrap_or(0);
            tracker.totals.codex = tracker.totals.codex.saturating_add(total);
        });
    }

    fn read_lines(&mut self, path: &Path, mut consume: impl FnMut(&mut Self, &Value)) {
        let Ok(mut file) = File::open(path) else {
            return;
        };
        let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        let offset = self.offsets.get(path).copied().unwrap_or(0).min(length);
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return;
        }
        let mut reader = BufReader::new(file);
        let mut bytes = offset;
        let mut line = String::new();
        // Bound any single file so a large log cannot monopolize the worker.
        for _ in 0..5000 {
            line.clear();
            let Ok(read) = reader.read_line(&mut line) else {
                break;
            };
            if read == 0 {
                break;
            }
            bytes += read as u64;
            if let Ok(value) = serde_json::from_str(&line) {
                consume(self, &value);
            }
        }
        self.offsets.insert(path.to_path_buf(), bytes);
    }
}

fn visit_jsonl(dir: &Path, files: &mut Vec<PathBuf>, cap: usize) {
    if files.len() >= cap {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if files.len() >= cap {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            visit_jsonl(&path, files, cap);
        } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl")
            && path
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .is_some_and(|modified| {
                    local_day(system_time(modified)) == local_day(system_time(SystemTime::now()))
                })
        {
            files.push(path);
        }
    }
}

fn system_time(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn local_day(unix: i64) -> (u16, u16, u16) {
    unsafe {
        let ticks = (unix + 11_644_473_600) as u64 * 10_000_000;
        let file = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        let mut local: SYSTEMTIME = std::mem::zeroed();
        FileTimeToSystemTime(&file, &mut utc);
        SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local);
        (local.wYear, local.wMonth, local.wDay)
    }
}
