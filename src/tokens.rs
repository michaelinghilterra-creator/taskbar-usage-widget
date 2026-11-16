use crate::value::{day_string, project_key, Pricing, Snapshot, Store, Usage};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub claude: u64,
    pub claude_output: u64,
    pub codex: u64,
}

#[derive(Default)]
struct CodexContext {
    model: String,
    cwd: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileKind {
    Claude,
    Codex,
}

#[derive(Clone, Debug)]
struct Candidate {
    path: PathBuf,
    modified: SystemTime,
    length: u64,
    kind: FileKind,
}

#[derive(Default)]
pub struct Tracker {
    store: Store,
    totals: Totals,
    snapshot: Snapshot,
    codex: HashMap<PathBuf, CodexContext>,
}

impl Tracker {
    pub fn poll(&mut self, account_dirs: &[PathBuf], now: i64) -> Totals {
        self.store.ensure_loaded();
        let today = local_day(now);
        let cutoff = now - 31 * 86400;
        let mut candidates = collect_claude_files(account_dirs, cutoff);
        let codex_cutoff = UNIX_EPOCH + std::time::Duration::from_secs(cutoff.max(0) as u64);
        candidates.extend(
            crate::providers::value_codex_files(codex_cutoff)
                .into_iter()
                .map(|(path, modified, length)| Candidate {
                    path,
                    modified,
                    length,
                    kind: FileKind::Codex,
                }),
        );
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.modified));

        let pricing = Pricing::load();
        let started = Instant::now();
        let mut did_work = false;
        for candidate in &candidates {
            if !file_has_unread_bytes(&self.store, candidate) {
                continue;
            }
            if did_work && started.elapsed() >= Duration::from_millis(200) {
                break;
            }
            match candidate.kind {
                FileKind::Claude => {
                    self.read_claude(&candidate.path, cutoff, &pricing);
                }
                FileKind::Codex => {
                    self.read_codex(&candidate.path, cutoff, &pricing);
                }
            }
            did_work = true;
        }
        self.store.backfill_done = backfill_complete(&self.store, &candidates);
        self.store.retain(today);
        self.snapshot = self.store.snapshot(today, &pricing);
        let claude = self.store.today_usage("claude", today);
        let codex = self.store.today_usage("codex", today);
        self.totals = Totals {
            claude: claude.total(),
            claude_output: claude.output,
            codex: codex.total(),
        };
        let _ = self.store.save();
        self.totals
    }

    pub fn value_snapshot(&self) -> Snapshot {
        self.snapshot.clone()
    }

    fn read_claude(&mut self, path: &Path, cutoff: i64, pricing: &Pricing) -> bool {
        self.read_lines(
            path,
            |line| line.contains("usage"),
            |tracker, value| {
                let Some(timestamp) = value["timestamp"]
                    .as_str()
                    .and_then(crate::api::parse_iso8601)
                else {
                    return;
                };
                if timestamp < cutoff || value["message"]["usage"].is_null() {
                    return;
                }
                let day = day_string(local_day(timestamp));
                let id = value["message"]["id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{}:{timestamp}", path.display()));
                if !tracker.store.mark_seen(&id, &day) {
                    return;
                }
                let model = value["message"]["model"]
                    .as_str()
                    .or_else(|| value["model"].as_str())
                    .unwrap_or("unknown");
                if model == "<synthetic>" {
                    return;
                }
                let source = &value["message"]["usage"];
                let usage = claude_usage(source);
                let usd = pricing.claude_cost(
                    model,
                    usage,
                    source["speed"].as_str() == Some("fast"),
                    source["inference_geo"].as_str() == Some("us"),
                );
                tracker.store.add(
                    &day,
                    "claude",
                    model,
                    &project_key(value["cwd"].as_str()),
                    usage,
                    usd,
                    usd.is_some(),
                );
            },
        )
    }

    fn read_codex(&mut self, path: &Path, cutoff: i64, pricing: &Pricing) -> bool {
        if !self.codex.contains_key(path) && self.store.offset(path) > 0 {
            self.seed_codex_context(path);
        }
        self.read_lines(
            path,
            |line| {
                line.contains("token_count")
                    || line.contains("turn_context")
                    || line.contains("session_meta")
            },
            |tracker, value| {
                let kind = value["type"].as_str();
                let payload = &value["payload"];
                let context = tracker.codex.entry(path.to_owned()).or_default();
                if kind == Some("session_meta") {
                    context.cwd = payload["cwd"].as_str().map(str::to_owned);
                    return;
                }
                if kind == Some("turn_context") {
                    if let Some(model) = payload["model"].as_str() {
                        context.model = model.to_owned();
                    }
                    if let Some(cwd) = payload["cwd"].as_str() {
                        context.cwd = Some(cwd.to_owned());
                    }
                    return;
                }
                let Some(timestamp) = value["timestamp"]
                    .as_str()
                    .and_then(crate::api::parse_iso8601)
                else {
                    return;
                };
                if timestamp < cutoff || payload["type"].as_str() != Some("token_count") {
                    return;
                }
                let token = &payload["info"]["last_token_usage"];
                let input = token["input_tokens"].as_u64().unwrap_or(0);
                let cached = token["cached_input_tokens"]
                    .as_u64()
                    .unwrap_or(0)
                    .min(input);
                let output = token["output_tokens"].as_u64().unwrap_or(0);
                let usage = Usage {
                    input: input.saturating_sub(cached),
                    cache_read: cached,
                    output,
                    ..Default::default()
                };
                let model = if context.model.is_empty() {
                    "unknown"
                } else {
                    &context.model
                };
                if model == "<synthetic>" {
                    return;
                }
                let usd = pricing.codex_cost(model, input, cached, output);
                tracker.store.add(
                    &day_string(local_day(timestamp)),
                    "codex",
                    model,
                    &project_key(context.cwd.as_deref()),
                    usage,
                    usd,
                    usd.is_some(),
                );
            },
        )
    }

    fn seed_codex_context(&mut self, path: &Path) {
        let Ok(file) = File::open(path) else { return };
        let limit = self.store.offset(path);
        let mut bytes = 0u64;
        let mut context = CodexContext::default();
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            bytes += line.len() as u64 + 1;
            if bytes > limit {
                break;
            }
            if !line.contains("turn_context") && !line.contains("session_meta") {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let payload = &value["payload"];
            match value["type"].as_str() {
                Some("session_meta") => context.cwd = payload["cwd"].as_str().map(str::to_owned),
                Some("turn_context") => {
                    if let Some(model) = payload["model"].as_str() {
                        context.model = model.to_owned();
                    }
                    if let Some(cwd) = payload["cwd"].as_str() {
                        context.cwd = Some(cwd.to_owned());
                    }
                }
                _ => {}
            }
        }
        self.codex.insert(path.to_owned(), context);
    }

    fn read_lines(
        &mut self,
        path: &Path,
        relevant: impl Fn(&str) -> bool,
        mut consume: impl FnMut(&mut Self, &Value),
    ) -> bool {
        let Ok(mut file) = File::open(path) else {
            return true;
        };
        let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        let stored_offset = self.store.offset(path);
        let offset = if stored_offset > length {
            0
        } else {
            stored_offset
        };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return true;
        }
        let mut reader = BufReader::new(file);
        let mut bytes = offset;
        let mut line = String::new();
        for _ in 0..5000 {
            line.clear();
            let Ok(read) = reader.read_line(&mut line) else {
                break;
            };
            if read == 0 {
                break;
            }
            bytes += read as u64;
            if relevant(&line) {
                if let Ok(value) = serde_json::from_str(&line) {
                    consume(self, &value);
                }
            }
        }
        self.store.set_offset(path, bytes);
        bytes >= length
    }
}

fn claude_usage(source: &Value) -> Usage {
    let split = &source["cache_creation"];
    Usage {
        input: source["input_tokens"].as_u64().unwrap_or(0),
        cache_write_5m: split["ephemeral_5m_input_tokens"].as_u64().unwrap_or(0),
        cache_write_1h: if split.is_object() {
            split["ephemeral_1h_input_tokens"].as_u64().unwrap_or(0)
        } else {
            source["cache_creation_input_tokens"].as_u64().unwrap_or(0)
        },
        cache_read: source["cache_read_input_tokens"].as_u64().unwrap_or(0),
        output: source["output_tokens"].as_u64().unwrap_or(0),
    }
}

fn collect_claude_files(account_dirs: &[PathBuf], cutoff: i64) -> Vec<Candidate> {
    let mut files = Vec::new();
    for dir in account_dirs {
        visit_jsonl(&dir.join("projects"), &mut files, cutoff);
    }
    files.sort_by_key(|candidate| std::cmp::Reverse(candidate.modified));
    files
}

fn visit_jsonl(dir: &Path, files: &mut Vec<Candidate>, cutoff: i64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            visit_jsonl(&path, files, cutoff);
        } else if path.extension().and_then(|v| v.to_str()) == Some("jsonl")
            && metadata
                .modified()
                .ok()
                .is_some_and(|modified| system_time(modified) >= cutoff)
        {
            files.push(Candidate {
                path,
                modified: metadata.modified().unwrap_or(UNIX_EPOCH),
                length: metadata.len(),
                kind: FileKind::Claude,
            });
        }
    }
}

fn file_has_unread_bytes(store: &Store, candidate: &Candidate) -> bool {
    let length = candidate
        .path
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(candidate.length);
    store.offset(&candidate.path) != length
}

fn backfill_complete(store: &Store, candidates: &[Candidate]) -> bool {
    !candidates
        .iter()
        .any(|candidate| file_has_unread_bytes(store, candidate))
}

fn system_time(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn local_day(unix: i64) -> (i32, u32, u32) {
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
        (local.wYear as i32, local.wMonth as u32, local.wDay as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn pricing() -> Pricing {
        Pricing::embedded_for_test()
    }

    #[test]
    fn claude_streamed_lines_are_deduped() {
        let dir = std::env::temp_dir().join(format!("widget-value-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("fixture.jsonl");
        let mut f = File::create(&path).unwrap();
        let line = r#"{"timestamp":"2026-11-16T12:00:00Z","cwd":"C:\\work\\alpha","message":{"id":"msg_test_1","model":"claude-sonnet-4-5","usage":{"input_tokens":10,"output_tokens":5}}}"#;
        writeln!(f, "{line}").unwrap();
        writeln!(f, "{line}").unwrap();
        let mut tracker = Tracker::default();
        tracker.store = Store::empty_for_test();
        tracker.read_claude(&path, 0, &pricing());
        let day = local_day(crate::api::parse_iso8601("2026-11-16T12:00:00Z").unwrap());
        assert_eq!(tracker.store.today_usage("claude", day).total(), 15);
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn claude_cache_split_and_fallback_are_parsed() {
        let split = json!({"input_tokens":1,"output_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":99,"cache_creation":{"ephemeral_5m_input_tokens":4,"ephemeral_1h_input_tokens":5}});
        assert_eq!(
            claude_usage(&split),
            Usage {
                input: 1,
                output: 2,
                cache_read: 3,
                cache_write_5m: 4,
                cache_write_1h: 5
            }
        );
        let fallback = json!({"cache_creation_input_tokens":7});
        assert_eq!(claude_usage(&fallback).cache_write_1h, 7);
    }

    #[test]
    fn newest_file_after_more_than_300_visited_entries_is_read_first() {
        let root =
            std::env::temp_dir().join(format!("widget-value-many-files-{}", std::process::id()));
        let account = root.join("account");
        let old_dir = account.join("projects").join("a-old");
        let newest_dir = account.join("projects").join("z-last").join("subagents");
        fs::create_dir_all(&old_dir).unwrap();
        fs::create_dir_all(&newest_dir).unwrap();
        let old_time = UNIX_EPOCH + Duration::from_secs(1_000);
        for index in 0..301 {
            let path = old_dir.join(format!("old-{index:03}.jsonl"));
            let file = File::create(path).unwrap();
            file.set_times(std::fs::FileTimes::new().set_modified(old_time))
                .unwrap();
        }
        let newest = newest_dir.join("newest.jsonl");
        let mut file = File::create(&newest).unwrap();
        writeln!(file, r#"{{"timestamp":"2026-11-16T12:00:00Z","cwd":"C:\\work\\newest","message":{{"id":"msg_newest","model":"claude-sonnet-4-5","usage":{{"input_tokens":11,"output_tokens":4}}}}}}"#).unwrap();
        file.set_times(
            std::fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(2_000)),
        )
        .unwrap();

        let mut visited = Vec::new();
        visit_jsonl(&account.join("projects"), &mut visited, 0);
        assert_eq!(visited.len(), 302);
        assert_eq!(visited.last().map(|item| &item.path), Some(&newest));

        let candidates = collect_claude_files(&[account], 0);
        assert_eq!(candidates.first().map(|item| &item.path), Some(&newest));
        let mut tracker = Tracker::default();
        tracker.store = Store::empty_for_test();
        tracker.read_claude(&candidates[0].path, 0, &pricing());
        let day = local_day(crate::api::parse_iso8601("2026-11-16T12:00:00Z").unwrap());
        assert_eq!(tracker.store.today_usage("claude", day).total(), 15);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backfill_stays_incomplete_while_any_file_has_unread_bytes() {
        let root =
            std::env::temp_dir().join(format!("widget-value-unread-file-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let complete_path = root.join("complete.jsonl");
        let unread_path = root.join("unread.jsonl");
        fs::write(&complete_path, "complete\n").unwrap();
        fs::write(&unread_path, "unread\n").unwrap();
        let candidates = [complete_path, unread_path]
            .into_iter()
            .map(|path| Candidate {
                length: path.metadata().unwrap().len(),
                modified: UNIX_EPOCH,
                path,
                kind: FileKind::Claude,
            })
            .collect::<Vec<_>>();
        let mut store = Store::empty_for_test();
        store.set_offset(&candidates[0].path, candidates[0].length);
        assert!(!backfill_complete(&store, &candidates));
        store.set_offset(&candidates[1].path, candidates[1].length);
        assert!(backfill_complete(&store, &candidates));
        fs::remove_dir_all(root).unwrap();
    }
}
