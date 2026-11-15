use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Claude,
    Codex,
    Spark,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub key: String,
    pub label: String,
    pub percent: f64,
    pub resets_at: Option<i64>,
    pub window_secs: Option<i64>,
    pub reset_since_last_run: bool,
}

#[derive(Clone, Debug)]
pub struct Reading {
    pub provider: Provider,
    pub account: Option<String>,
    pub windows: Vec<Window>,
    pub fetched_at: i64,
    pub as_of: i64,
    pub error: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pace {
    pub elapsed_frac: f64,
    pub elapsed_pct: f64,
    pub ahead_by: f64,
    pub projected: f64,
    pub full_at: Option<i64>,
}

pub fn pace(
    percent: f64,
    resets_at: Option<i64>,
    window_secs: Option<i64>,
    now: i64,
    recent_slope_per_sec: Option<f64>,
) -> Pace {
    let elapsed_frac = match (resets_at, window_secs.filter(|s| *s > 0)) {
        (Some(reset), Some(seconds)) => {
            (1.0 - (reset - now) as f64 / seconds as f64).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };
    let elapsed_pct = elapsed_frac * 100.0;
    let projected = (percent / elapsed_frac.max(0.02)).min(999.0);
    let avg_slope = if elapsed_frac > 0.0 {
        percent / (elapsed_frac * window_secs.unwrap_or(1) as f64)
    } else {
        0.0
    };
    let slope = recent_slope_per_sec
        .filter(|s| *s > 0.0)
        .unwrap_or(avg_slope);
    let full_at = resets_at.and_then(|reset| {
        if slope <= 0.0 || percent >= 100.0 {
            return None;
        }
        let at = now + ((100.0 - percent) / slope).ceil() as i64;
        (at < reset).then_some(at)
    });
    Pace {
        elapsed_frac,
        elapsed_pct,
        ahead_by: percent - elapsed_pct,
        projected,
        full_at,
    }
}

pub fn recent_slope(
    samples: &[(i64, f64, Option<i64>)],
    reset: Option<i64>,
    now: i64,
) -> Option<f64> {
    let mut points = samples
        .iter()
        .filter(|(t, _, sample_reset)| *t >= now - 1800 && *sample_reset == reset);
    let first = points.next()?;
    let last = points.last().unwrap_or(first);
    let elapsed = last.0 - first.0;
    (elapsed > 0)
        .then_some((last.1 - first.1) / elapsed as f64)
        .filter(|slope| *slope > 0.0)
}

pub fn resets_in(seconds: i64) -> String {
    let minutes = (seconds.max(0) + 59) / 60;
    if minutes >= 1440 {
        format!("{} d {} h", minutes / 1440, minutes % 1440 / 60)
    } else if minutes >= 60 {
        format!("{} h {} m", minutes / 60, minutes % 60)
    } else {
        format!("{} m", minutes)
    }
}

pub fn forecast_wording(pace: Pace, _reset: Option<i64>) -> String {
    if let Some(at) = pace.full_at {
        format!("At this pace: empty ~{}", crate::local_time(at, true))
    } else {
        format!("On pace to end at ~{} %", pace.projected.round() as i64)
    }
}

fn unix(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_else(|| ".".into()))
                .join(".codex")
        })
}

fn visit_rollouts(dir: &Path, cutoff: SystemTime, files: &mut Vec<(SystemTime, u64, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            visit_rollouts(&path, cutoff, files);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        {
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
            if modified >= cutoff {
                files.push((modified, metadata.len(), path));
            }
        }
    }
}

pub fn recent_codex_files() -> Vec<(PathBuf, SystemTime, u64)> {
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(2 * 86400))
        .unwrap_or(UNIX_EPOCH);
    let mut files = Vec::new();
    visit_rollouts(&codex_home().join("sessions"), cutoff, &mut files);
    files.sort_by_key(|item| std::cmp::Reverse(item.0));
    files
        .into_iter()
        .take(5)
        .map(|(modified, size, path)| (path, modified, size))
        .collect()
}

pub fn codex_files_signature() -> Vec<(PathBuf, u64, i64)> {
    recent_codex_files()
        .into_iter()
        .map(|(path, modified, size)| (path, size, unix(modified)))
        .collect()
}

fn latest_reading_in_file(
    path: &Path,
    modified: SystemTime,
    size: u64,
    now: i64,
) -> Result<Option<Reading>, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let start = size.saturating_sub(512 * 1024);
    file.seek(SeekFrom::Start(start))
        .map_err(|error| error.to_string())?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    let mut lines = text.lines();
    if start > 0 {
        lines.next();
    }
    Ok(lines
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|value| parse_codex_line(&value, now, unix(modified)))
        .max_by_key(|reading| reading.as_of))
}

pub fn read_codex(now: i64) -> Reading {
    let files = recent_codex_files();
    if files.is_empty() {
        return Reading {
            provider: Provider::Codex,
            account: None,
            windows: Vec::new(),
            fetched_at: now,
            as_of: now,
            error: Some("No Codex session modified in the last two days".into()),
            plan_type: None,
        };
    }
    let mut latest = None;
    let mut last_error = None;
    for (path, modified, size) in files {
        match latest_reading_in_file(&path, modified, size, now) {
            Ok(Some(reading))
                if latest
                    .as_ref()
                    .is_none_or(|old: &Reading| reading.as_of > old.as_of) =>
            {
                latest = Some(reading);
            }
            Ok(_) => {}
            Err(error) => last_error = Some(error),
        }
    }
    latest.unwrap_or_else(|| Reading {
        provider: Provider::Codex,
        account: None,
        windows: Vec::new(),
        fetched_at: now,
        as_of: now,
        error: Some(last_error.unwrap_or_else(|| "No Codex rate-limit event found".into())),
        plan_type: None,
    })
}

pub fn parse_codex_line(value: &Value, now: i64, fallback_as_of: i64) -> Option<Reading> {
    let payload = &value["payload"];
    if payload["type"].as_str() != Some("token_count") || payload["rate_limits"].is_null() {
        return None;
    }
    let rate_limits = &payload["rate_limits"];
    let mut windows = Vec::new();
    for (name, data) in [
        ("primary", &rate_limits["primary"]),
        ("secondary", &rate_limits["secondary"]),
    ] {
        if data.is_null() {
            continue;
        }
        let minutes = data["window_minutes"].as_i64()?;
        let resets_at = data["resets_at"].as_i64();
        let reset_since_last_run = resets_at.is_some_and(|reset| reset < now);
        let key = match minutes {
            300 => "5h".into(),
            10080 => "week".into(),
            _ if minutes % 60 == 0 => format!("{}h", minutes / 60),
            _ => format!("{}m", minutes),
        };
        windows.push(Window {
            key,
            label: if name == "primary" {
                "Primary".into()
            } else {
                "Secondary".into()
            },
            percent: if reset_since_last_run {
                0.0
            } else {
                data["used_percent"].as_f64().unwrap_or(0.0)
            },
            resets_at,
            window_secs: Some(minutes * 60),
            reset_since_last_run,
        });
    }
    let as_of = value["timestamp"]
        .as_str()
        .and_then(crate::api::parse_iso8601)
        .unwrap_or(fallback_as_of);
    Some(Reading {
        provider: Provider::Codex,
        account: None,
        windows,
        fetched_at: now,
        as_of,
        error: None,
        plan_type: rate_limits["plan_type"].as_str().map(str::to_owned),
    })
}

#[derive(Clone, Debug, Default)]
pub struct SparkSnapshot {
    pub fetched_at: i64,
    pub up: bool,
    pub tok_s: f64,
    pub gpu_util: f64,
    pub busy: bool,
    pub endpoints: Vec<Value>,
    pub sys: Value,
    pub host: Value,
    pub aux: Vec<Value>,
    pub error: Option<String>,
}

pub fn parse_spark(value: &Value, now: i64) -> SparkSnapshot {
    let _provider = Provider::Spark;
    let endpoints = value["endpoints"].as_array().cloned().unwrap_or_default();
    let tok_s = endpoints
        .iter()
        .filter_map(|endpoint| endpoint["tok_s"].as_f64())
        .sum();
    let running: i64 = endpoints
        .iter()
        .filter_map(|endpoint| endpoint["running"].as_i64())
        .sum();
    let gpu_util = value["sys"]["gpu"]["util"].as_f64().unwrap_or(0.0);
    let up = endpoints
        .iter()
        .any(|endpoint| endpoint["up"].as_bool().unwrap_or(false));
    SparkSnapshot {
        fetched_at: now,
        up,
        tok_s,
        gpu_util,
        busy: running > 0 || gpu_util > 30.0,
        endpoints,
        sys: value["sys"].clone(),
        host: value["host"].clone(),
        aux: value["aux"].as_array().cloned().unwrap_or_default(),
        error: None,
    }
}

pub fn fetch_spark(host: &str, now: i64) -> SparkSnapshot {
    match crate::http::request_to(
        "GET",
        host,
        80,
        "/api/stats",
        "Accept: application/json",
        None,
        false,
        2500,
    ) {
        Ok(response) if response.status == 200 => serde_json::from_slice::<Value>(&response.body)
            .map(|value| parse_spark(&value, now))
            .unwrap_or_else(|_| SparkSnapshot {
                fetched_at: now,
                error: Some("Bad Spark response".into()),
                ..Default::default()
            }),
        Ok(response) => SparkSnapshot {
            fetched_at: now,
            error: Some(format!("Spark HTTP {}", response.status)),
            ..Default::default()
        },
        Err(error) => SparkSnapshot {
            fetched_at: now,
            error: Some(error),
            ..Default::default()
        },
    }
}

pub fn window_json(window: &Window, now: i64, recent: Option<f64>) -> Value {
    let pace = pace(
        window.percent,
        window.resets_at,
        window.window_secs,
        now,
        recent,
    );
    json!({
        "key": window.key,
        "label": window.label,
        "percent": window.percent,
        "resets_at": window.resets_at,
        "window_secs": window.window_secs,
        "elapsed_pct": pace.elapsed_pct,
        "ahead_by": pace.ahead_by,
        "projected": pace.projected,
        "full_at": pace.full_at,
        "reset_since_last_run": window.reset_since_last_run
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pace_math() {
        let pace = pace(60.0, Some(2000), Some(1000), 1500, None);
        assert_eq!(pace.elapsed_pct, 50.0);
        assert_eq!(pace.ahead_by, 10.0);
        assert_eq!(pace.projected, 120.0);
        assert_eq!(pace.full_at, Some(1834));
    }

    #[test]
    fn slope_math() {
        assert_eq!(
            recent_slope(&[(100, 10.0, Some(9)), (200, 20.0, Some(9))], Some(9), 200),
            Some(0.1)
        );
    }

    #[test]
    fn codex_parse() {
        let value = json!({"timestamp":"1970-01-01T00:16:40Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":9.0,"window_minutes":10080,"resets_at":2000},"secondary":null,"plan_type":"plus"}}});
        let reading = parse_codex_line(&value, 1500, 1).unwrap();
        assert_eq!(reading.windows[0].key, "week");
        assert_eq!(reading.plan_type.as_deref(), Some("plus"));
        let reset = parse_codex_line(&value, 2500, 1).unwrap();
        assert_eq!(reset.windows[0].percent, 0.0);
        assert!(reset.windows[0].reset_since_last_run);
    }

    #[test]
    fn spark_missing_fields() {
        let spark = parse_spark(&json!({"endpoints":[{"up":true,"tok_s":2.5}]}), 1);
        assert_eq!(spark.tok_s, 2.5);
        assert_eq!(spark.gpu_util, 0.0);
    }
}
