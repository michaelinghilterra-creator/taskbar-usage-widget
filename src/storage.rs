use crate::providers::{self, Reading, SparkSnapshot};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use windows_sys::Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_WRITE_THROUGH};

pub struct History {
    entries: Vec<Value>,
    last: HashMap<String, (f64, i64)>,
}

impl History {
    pub fn load(now: i64) -> Result<Self, String> {
        fs::create_dir_all(dir()).map_err(|error| error.to_string())?;
        let cutoff = now - 8 * 86400;
        let mut entries = Vec::new();
        if let Ok(file) = fs::File::open(history_path()) {
            entries = BufReader::new(file)
                .lines()
                .map_while(Result::ok)
                .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
                .filter(|entry| entry["t"].as_i64().is_some_and(|time| time >= cutoff))
                .collect();
        }
        let mut history = Self {
            entries,
            last: HashMap::new(),
        };
        history.rebuild_last();
        history.rewrite()?;
        Ok(history)
    }

    fn rebuild_last(&mut self) {
        for entry in &self.entries {
            let Some(provider) = entry["p"].as_str() else {
                continue;
            };
            let Some(time) = entry["t"].as_i64() else {
                continue;
            };
            if provider == "spark" || provider.starts_with("local:") {
                self.last.insert(
                    provider.into(),
                    (entry["tok"].as_f64().unwrap_or(0.0), time),
                );
            } else if let (Some(key), Some(value)) = (entry["k"].as_str(), entry["v"].as_f64()) {
                self.last.insert(format!("{provider}:{key}"), (value, time));
            }
        }
    }

    fn rewrite(&self) -> Result<(), String> {
        let bytes = self
            .entries
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + if self.entries.is_empty() { "" } else { "\n" };
        let path = history_path();
        let temporary = path.with_extension("jsonl.tmp");
        fs::write(&temporary, bytes)
            .and_then(|_| atomic_replace(&temporary, &path))
            .map_err(|error| error.to_string())
    }

    pub fn append(
        &mut self,
        readings: &[Reading],
        local_servers: &[SparkSnapshot],
        now: i64,
    ) -> Result<(), String> {
        let mut new_entries = Vec::new();
        for reading in readings {
            let provider = provider_name(reading.provider);
            for window in &reading.windows {
                let key = format!("{provider}:{}", window.key);
                let due = self.last.get(&key).is_none_or(|(value, time)| {
                    (*value - window.percent).abs() > 0.0001 || now - *time >= 600
                });
                if due {
                    new_entries.push(json!({
                        "t": now,
                        "p": provider,
                        "k": window.key,
                        "v": window.percent,
                        "r": window.resets_at
                    }));
                    self.last.insert(key, (window.percent, now));
                }
            }
        }
        for spark in local_servers {
            let history_key = format!("local:{}", spark.name);
            let due = self
                .last
                .get(&history_key)
                .is_none_or(|(_, time)| now - *time >= 60);
            if due {
                new_entries.push(json!({
                    "t": now,
                    "p": history_key,
                    "tok": spark.tok_s,
                    "gpu": spark.gpu_util
                }));
                self.last.insert(history_key, (spark.tok_s, now));
            }
        }
        if new_entries.is_empty() {
            return Ok(());
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(history_path())
            .map_err(|error| error.to_string())?;
        for entry in &new_entries {
            writeln!(file, "{entry}").map_err(|error| error.to_string())?;
        }
        self.entries.extend(new_entries);
        Ok(())
    }

    pub fn samples(&self, provider: &str, key: &str) -> Vec<(i64, f64, Option<i64>)> {
        self.entries
            .iter()
            .filter(|entry| {
                entry["p"].as_str() == Some(provider) && entry["k"].as_str() == Some(key)
            })
            .filter_map(|entry| {
                Some((
                    entry["t"].as_i64()?,
                    entry["v"].as_f64()?,
                    entry["r"].as_i64(),
                ))
            })
            .collect()
    }
}

fn atomic_replace(temporary: &Path, path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return fs::rename(temporary, path);
    }
    let destination = crate::http::wide(&path.to_string_lossy());
    let source = crate::http::wide(&temporary.to_string_lossy());
    let replaced = unsafe {
        ReplaceFileW(
            destination.as_ptr(),
            source.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if replaced != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes).and_then(|_| atomic_replace(&temporary, path))
}

pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into()))
        .join("TaskbarUsageWidget")
}

fn state_path() -> PathBuf {
    dir().join("state.json")
}

fn history_path() -> PathBuf {
    dir().join("history.jsonl")
}

fn provider_name(provider: providers::Provider) -> &'static str {
    match provider {
        providers::Provider::Claude => "claude",
        providers::Provider::Codex => "codex",
        providers::Provider::Spark => "spark",
    }
}

pub fn write_state(
    readings: &[Reading],
    local_servers: &[SparkSnapshot],
    history: &History,
    now: i64,
    tokens: (u64, u64, u64),
    tokens_checked_at: i64,
) -> Result<(), String> {
    fs::create_dir_all(dir()).map_err(|error| error.to_string())?;
    let readings_json: Vec<Value> = readings
        .iter()
        .map(|reading| {
            let provider = provider_name(reading.provider);
            let windows = reading
                .windows
                .iter()
                .map(|window| {
                    let samples = history.samples(provider, &window.key);
                    let slope = providers::recent_slope(&samples, window.resets_at, now);
                    providers::window_json(window, now, slope)
                })
                .collect::<Vec<_>>();
            json!({
                "provider": provider,
                "account": reading.account,
                "fetched_at": reading.fetched_at,
                "as_of": reading.as_of,
                "error": reading.error,
                "plan_type": reading.plan_type,
                "live": reading.live,
                "windows": windows
            })
        })
        .collect();
    let server_json = |spark: &SparkSnapshot| {
        json!({
            "name": spark.name,
            "type": spark.engine,
            "fetched_at": spark.fetched_at,
            "up": spark.up,
            "tok_s": spark.tok_s,
            "gpu_util": spark.gpu_util,
            "busy": spark.busy,
            "endpoints": spark.endpoints,
            "sys": spark.sys,
            "host": spark.host,
            "aux": spark.aux,
            "error": spark.error
        })
    };
    let local_servers_json = local_servers.iter().map(server_json).collect::<Vec<_>>();
    let spark_json = local_servers.first().map(server_json);
    let bytes = serde_json::to_vec(&json!({
        "written_at": now,
        "readings": readings_json,
        "spark": spark_json,
        "local_servers": local_servers_json,
        "tokens": {
            "claude": tokens.0,
            "claude_output": tokens.1,
            "codex": tokens.2,
            "checked_at": tokens_checked_at
        }
    }))
    .unwrap();
    let path = state_path();
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)
        .and_then(|_| atomic_replace(&temporary, &path))
        .map_err(|error| error.to_string())
}
