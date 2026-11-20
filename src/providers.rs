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
    pub card_only: bool,
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
    pub live: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexFetchStatus {
    Live,
    Fallback,
    Unauthorized,
    RateLimited,
}

pub struct CodexFetch {
    pub reading: Reading,
    pub status: CodexFetchStatus,
    pub auth_mtime: Option<i64>,
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

fn codex_auth_path() -> PathBuf {
    codex_home().join("auth.json")
}

pub fn codex_auth_mtime() -> Option<i64> {
    fs::metadata(codex_auth_path())
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(unix)
}

#[derive(Debug, PartialEq, Eq)]
struct CodexCredentials {
    access_token: String,
    account_id: String,
}

fn parse_codex_auth(text: &str) -> Option<CodexCredentials> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value["auth_mode"].as_str()? != "chatgpt" {
        return None;
    }
    let access_token = value["tokens"]["access_token"].as_str()?.trim();
    let account_id = value["tokens"]["account_id"].as_str()?.trim();
    if access_token.is_empty() || account_id.is_empty() {
        return None;
    }
    Some(CodexCredentials {
        access_token: access_token.to_owned(),
        account_id: account_id.to_owned(),
    })
}

fn decode_base64url(value: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(value.len() * 3 / 4);
    let mut bits = 0u32;
    let mut count = 0u32;
    for byte in value.bytes() {
        if byte == b'=' {
            break;
        }
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32;
        bits = (bits << 6) | digit;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1u32 << count).saturating_sub(1);
        }
    }
    Some(out)
}

fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let decoded = decode_base64url(payload)?;
    serde_json::from_slice::<Value>(&decoded).ok()?["exp"].as_i64()
}

fn chatgpt_base_url(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        if key.trim() != "chatgpt_base_url" {
            return None;
        }
        let value = value.trim();
        if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
            return None;
        }
        let value = value[1..value.len() - 1].trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn codex_usage_url_from_config(config: Option<&str>) -> String {
    let base = config.and_then(chatgpt_base_url);
    match base {
        Some(base) if base.contains("/backend-api") => {
            format!("{}/wham/usage", base.trim_end_matches('/'))
        }
        Some(base) => format!("{}/api/codex/usage", base.trim_end_matches('/')),
        None => "https://chatgpt.com/backend-api/wham/usage".into(),
    }
}

fn codex_usage_url() -> String {
    let config = fs::read_to_string(codex_home().join("config.toml")).ok();
    codex_usage_url_from_config(config.as_deref())
}

fn split_http_url(url: &str) -> Option<(String, u16, String, bool)> {
    let (secure, rest, port) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest, 443)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest, 80)
    } else {
        return None;
    };
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    if host.is_empty() {
        return None;
    }
    Some((host.to_owned(), port, format!("/{path}"), secure))
}

fn window_key(seconds: i64) -> String {
    match seconds {
        18_000 => "5h".into(),
        604_800 => "week".into(),
        2_592_000 => "month".into(),
        value if value > 0 && value % 86_400 == 0 => format!("{}d", value / 86_400),
        value if value > 0 && value % 3_600 == 0 => format!("{}h", value / 3_600),
        value => format!("{}h", value.max(0) / 3_600),
    }
}

/// Human label for a window key: "5-hour", "Weekly", "Monthly", else the key.
fn window_label(key: &str) -> String {
    match key {
        "5h" => "5-hour".into(),
        "week" => "Weekly".into(),
        "month" => "Monthly".into(),
        other => other.into(),
    }
}

fn parse_live_window(value: &Value, label: &str, card_only: bool, now: i64) -> Option<Window> {
    if value.is_null() {
        return None;
    }
    let seconds = value["limit_window_seconds"].as_i64()?;
    let resets_at = value["reset_at"].as_i64();
    let key = window_key(seconds);
    Some(Window {
        // Plan windows are named by their length; extra limits keep their own name.
        label: if card_only {
            label.to_owned()
        } else {
            window_label(&key)
        },
        key,
        percent: value["used_percent"].as_f64()?,
        resets_at,
        window_secs: Some(seconds),
        reset_since_last_run: resets_at.is_some_and(|reset| reset < now),
        card_only,
    })
}

fn parse_codex_usage(value: &Value, now: i64) -> Reading {
    let limits = &value["rate_limit"];
    let mut windows = Vec::new();
    if let Some(window) = parse_live_window(&limits["primary_window"], "Primary", false, now) {
        windows.push(window);
    }
    if let Some(window) = parse_live_window(&limits["secondary_window"], "Secondary", false, now) {
        windows.push(window);
    }
    if let Some(additional) = value["additional_rate_limits"].as_array() {
        for entry in additional {
            let Some(label) = entry["name"].as_str().or_else(|| entry["label"].as_str()) else {
                continue;
            };
            let candidates = [
                &entry["primary_window"],
                &entry["secondary_window"],
                &entry["rate_limit"]["primary_window"],
                &entry["rate_limit"]["secondary_window"],
                entry,
            ];
            if let Some(window) = candidates
                .into_iter()
                .find_map(|window| parse_live_window(window, label, true, now))
            {
                windows.push(window);
            }
        }
    }
    Reading {
        provider: Provider::Codex,
        account: None,
        windows,
        fetched_at: now,
        as_of: now,
        error: None,
        plan_type: value["plan_type"].as_str().map(str::to_owned),
        live: true,
    }
}

fn log_fallback(now: i64, error: Option<String>) -> Reading {
    let mut reading = read_codex(now);
    reading.live = false;
    if error.is_some() || reading.error.is_none() {
        reading.error = error;
    }
    reading
}

pub fn fetch_codex(now: i64, suppress_auth_mtime: Option<i64>) -> CodexFetch {
    const LOGIN_NOTE: &str = "Codex login needs refresh: run codex";
    let auth_path = codex_auth_path();
    let auth_mtime = fs::metadata(&auth_path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(unix);
    if suppress_auth_mtime.is_some() && suppress_auth_mtime == auth_mtime {
        return CodexFetch {
            reading: log_fallback(now, Some(LOGIN_NOTE.into())),
            status: CodexFetchStatus::Unauthorized,
            auth_mtime,
        };
    }
    let credentials = fs::read_to_string(auth_path)
        .ok()
        .and_then(|text| parse_codex_auth(&text));
    let Some(credentials) = credentials else {
        return CodexFetch {
            reading: log_fallback(now, None),
            status: CodexFetchStatus::Fallback,
            auth_mtime,
        };
    };
    if jwt_exp(&credentials.access_token).is_some_and(|expires| expires <= now + 300) {
        return CodexFetch {
            reading: log_fallback(now, Some(LOGIN_NOTE.into())),
            status: CodexFetchStatus::Unauthorized,
            auth_mtime,
        };
    }
    let url = codex_usage_url();
    // The login token is only ever sent over HTTPS, whatever config.toml says.
    let Some((host, port, path, secure)) = split_http_url(&url).filter(|parts| parts.3) else {
        return CodexFetch {
            reading: log_fallback(now, Some("Invalid Codex usage URL".into())),
            status: CodexFetchStatus::Fallback,
            auth_mtime,
        };
    };
    let headers = format!(
        "Authorization: Bearer {}\r\nChatGPT-Account-Id: {}\r\nUser-Agent: taskbar-usage-widget\r\nAccept: application/json",
        credentials.access_token, credentials.account_id
    );
    match crate::http::request_to("GET", &host, port, &path, &headers, None, secure, 15_000) {
        Ok(response) if response.status == 200 => {
            match serde_json::from_slice::<Value>(&response.body) {
                Ok(value) => CodexFetch {
                    reading: parse_codex_usage(&value, now),
                    status: CodexFetchStatus::Live,
                    auth_mtime,
                },
                Err(_) => CodexFetch {
                    reading: log_fallback(now, Some("Bad Codex usage response".into())),
                    status: CodexFetchStatus::Fallback,
                    auth_mtime,
                },
            }
        }
        Ok(response) if matches!(response.status, 401 | 403) => CodexFetch {
            reading: log_fallback(now, Some(LOGIN_NOTE.into())),
            status: CodexFetchStatus::Unauthorized,
            auth_mtime,
        },
        Ok(response) if response.status == 429 => CodexFetch {
            reading: log_fallback(now, Some("Codex usage is rate limited".into())),
            status: CodexFetchStatus::RateLimited,
            auth_mtime,
        },
        Ok(response) => CodexFetch {
            reading: log_fallback(now, Some(format!("Codex HTTP {}", response.status))),
            status: CodexFetchStatus::Fallback,
            auth_mtime,
        },
        Err(error) => CodexFetch {
            reading: log_fallback(now, Some(error)),
            status: CodexFetchStatus::Fallback,
            auth_mtime,
        },
    }
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

pub fn value_codex_files(cutoff: SystemTime) -> Vec<(PathBuf, SystemTime, u64)> {
    let mut files = Vec::new();
    visit_rollouts(&codex_home().join("sessions"), cutoff, &mut files);
    files.sort_by_key(|item| std::cmp::Reverse(item.0));
    files
        .into_iter()
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
            live: false,
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
        live: false,
    })
}

pub fn parse_codex_line(value: &Value, now: i64, fallback_as_of: i64) -> Option<Reading> {
    let payload = &value["payload"];
    if payload["type"].as_str() != Some("token_count") || payload["rate_limits"].is_null() {
        return None;
    }
    let rate_limits = &payload["rate_limits"];
    let mut windows = Vec::new();
    for data in [&rate_limits["primary"], &rate_limits["secondary"]] {
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
            label: window_label(&key),
            key,
            percent: if reset_since_last_run {
                0.0
            } else {
                data["used_percent"].as_f64().unwrap_or(0.0)
            },
            resets_at,
            window_secs: Some(minutes * 60),
            reset_since_last_run,
            card_only: false,
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
        live: false,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocalServerType {
    #[default]
    Auto,
    SparkDash,
    Sglang,
    Vllm,
    Llamacpp,
    Ollama,
    Lmstudio,
}

impl LocalServerType {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "spark-dash" => Self::SparkDash,
            "sglang" => Self::Sglang,
            "vllm" => Self::Vllm,
            "llamacpp" => Self::Llamacpp,
            "ollama" => Self::Ollama,
            "lmstudio" => Self::Lmstudio,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::SparkDash => "spark-dash",
            Self::Sglang => "sglang",
            Self::Vllm => "vllm",
            Self::Llamacpp => "llamacpp",
            Self::Ollama => "ollama",
            Self::Lmstudio => "lmstudio",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalServerConfig {
    pub name: String,
    pub url: String,
    pub server_type: LocalServerType,
}

pub fn parse_local_servers(text: &str) -> Result<Vec<LocalServerConfig>, String> {
    let mut servers = Vec::new();
    for (index, source) in text.lines().enumerate() {
        let line = source.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let fields = if line.contains('|') {
            line.split('|').map(str::trim).collect::<Vec<_>>()
        } else {
            line.split_whitespace().collect::<Vec<_>>()
        };
        if !(2..=3).contains(&fields.len()) {
            return Err(format!("Line {}: expected name url [type]", index + 1));
        }
        let name = fields[0];
        let url = fields[1].trim_end_matches('/');
        if name.is_empty() || name.contains('|') {
            return Err(format!("Line {}: invalid name", index + 1));
        }
        let Some((host, _, path, secure)) = split_http_url(url) else {
            return Err(format!("Line {}: URL must be http://host:port", index + 1));
        };
        if secure || host.is_empty() || path != "/" {
            return Err(format!("Line {}: URL must be HTTP with no path", index + 1));
        }
        let server_type = fields
            .get(2)
            .map_or(Some(LocalServerType::Auto), |value| {
                LocalServerType::parse(value)
            })
            .ok_or_else(|| format!("Line {}: unknown server type", index + 1))?;
        servers.push(LocalServerConfig {
            name: name.to_owned(),
            url: url.to_owned(),
            server_type,
        });
    }
    Ok(servers)
}

pub fn local_servers_registry(servers: &[LocalServerConfig]) -> String {
    servers
        .iter()
        .map(|server| {
            format!(
                "{}|{}|{}",
                server.name,
                server.url,
                server.server_type.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn migrate_spark_host(local_servers: Option<&str>, spark_host: Option<&str>) -> Option<String> {
    if local_servers.is_some_and(|value| !value.trim().is_empty()) {
        return None;
    }
    spark_host
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(|host| format!("Spark|http://{}|auto", host.trim_end_matches('/')))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CounterState {
    pub tokens: Option<(f64, i64)>,
    pub ttft_sum: Option<(f64, i64)>,
    pub ttft_count: Option<(f64, i64)>,
}

#[derive(Clone, Debug, Default)]
pub struct SparkSnapshot {
    pub name: String,
    pub engine: String,
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

fn prometheus_metrics(text: &str) -> std::collections::HashMap<String, f64> {
    let mut metrics = std::collections::HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(raw_name) = fields.next() else {
            continue;
        };
        let Some(value) = fields.next().and_then(|value| value.parse::<f64>().ok()) else {
            continue;
        };
        let name = raw_name.split('{').next().unwrap_or(raw_name);
        *metrics.entry(name.to_owned()).or_insert(0.0) += value;
    }
    metrics
}

fn counter_rate(previous: &mut Option<(f64, i64)>, current: f64, now: i64) -> f64 {
    let rate = previous
        .and_then(|(old, at)| {
            (current >= old && now > at).then_some((current - old) / (now - at) as f64)
        })
        .unwrap_or(0.0);
    *previous = Some((current, now));
    rate
}

pub fn parse_prometheus(
    name: &str,
    server_type: LocalServerType,
    text: &str,
    models: Vec<String>,
    now: i64,
    counters: &mut CounterState,
) -> SparkSnapshot {
    let metrics = prometheus_metrics(text);
    let get = |key: &str| metrics.get(key).copied();
    let (running, waiting, token_counter, throughput, kv, ttft_sum, ttft_count) = match server_type
    {
        LocalServerType::Sglang => (
            get("sglang:num_running_reqs"),
            get("sglang:num_queue_reqs"),
            get("sglang:generation_tokens_total"),
            get("sglang:gen_throughput"),
            get("sglang:token_usage"),
            get("sglang:time_to_first_token_seconds_sum"),
            get("sglang:time_to_first_token_seconds_count"),
        ),
        LocalServerType::Vllm => (
            get("vllm:num_requests_running"),
            get("vllm:num_requests_waiting"),
            get("vllm:generation_tokens_total"),
            None,
            get("vllm:kv_cache_usage_perc").or_else(|| get("vllm:gpu_cache_usage_perc")),
            get("vllm:time_to_first_token_seconds_sum"),
            get("vllm:time_to_first_token_seconds_count"),
        ),
        LocalServerType::Llamacpp => (
            get("llamacpp:requests_processing"),
            get("llamacpp:requests_deferred"),
            get("llamacpp:tokens_predicted_total"),
            None,
            get("llamacpp:kv_cache_usage_ratio"),
            None,
            None,
        ),
        _ => (None, None, None, None, None, None, None),
    };
    let tok_s = throughput.unwrap_or_else(|| {
        token_counter.map_or(0.0, |value| counter_rate(&mut counters.tokens, value, now))
    });
    let ttft_ms = match (ttft_sum, ttft_count) {
        (Some(sum), Some(count)) => {
            let old_sum = counters.ttft_sum;
            let old_count = counters.ttft_count;
            counters.ttft_sum = Some((sum, now));
            counters.ttft_count = Some((count, now));
            match (old_sum, old_count) {
                (Some((prior_sum, _)), Some((prior_count, _)))
                    if sum >= prior_sum && count > prior_count =>
                {
                    Some((sum - prior_sum) / (count - prior_count) * 1000.0)
                }
                _ => None,
            }
        }
        _ => None,
    };
    let running_value = running.unwrap_or(0.0).round() as i64;
    let endpoint = json!({
        "model": models.first(),
        "models": models,
        "engine": server_type.as_str(),
        "running": running.map(|value| value.round() as i64),
        "waiting": waiting.map(|value| value.round() as i64),
        "kv_pct": kv.map(|value| value * 100.0),
        "ttft_ms": ttft_ms
    });
    SparkSnapshot {
        name: name.to_owned(),
        engine: server_type.as_str().to_owned(),
        fetched_at: now,
        up: true,
        tok_s,
        busy: running_value > 0 || tok_s > 1.0,
        endpoints: vec![endpoint],
        ..Default::default()
    }
}

pub fn parse_ollama(name: &str, value: &Value, now: i64) -> SparkSnapshot {
    let models = value["models"].as_array().cloned().unwrap_or_default();
    let names = models
        .iter()
        .filter_map(|model| model["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let memory = models
        .iter()
        .filter_map(|model| model["size_vram"].as_u64())
        .sum::<u64>();
    SparkSnapshot {
        name: name.to_owned(),
        engine: "ollama".into(),
        fetched_at: now,
        up: true,
        endpoints: vec![json!({"model": names.first(), "models": names, "memory_bytes": memory})],
        host: json!({"memory_bytes": memory}),
        ..Default::default()
    }
}

pub fn parse_models(name: &str, engine: LocalServerType, value: &Value, now: i64) -> SparkSnapshot {
    let models = value["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| model["id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    SparkSnapshot {
        name: name.to_owned(),
        engine: engine.as_str().to_owned(),
        fetched_at: now,
        up: true,
        endpoints: vec![
            json!({"model": models.first(), "models": models, "engine": engine.as_str()}),
        ],
        ..Default::default()
    }
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
        name: "Spark".into(),
        engine: "spark-dash".into(),
        fetched_at: now,
        up,
        tok_s,
        gpu_util,
        busy: running > 0 || tok_s > 1.0 || gpu_util > 30.0,
        endpoints,
        sys: value["sys"].clone(),
        host: value["host"].clone(),
        aux: value["aux"].as_array().cloned().unwrap_or_default(),
        error: None,
    }
}

fn local_request(config: &LocalServerConfig, path: &str) -> Result<crate::http::Response, String> {
    let Some((host, port, _, secure)) = split_http_url(&config.url) else {
        return Err("Invalid local server URL".into());
    };
    crate::http::request_to(
        "GET",
        &host,
        port,
        path,
        "Accept: application/json, text/plain",
        None,
        secure,
        2500,
    )
}

fn response_json(response: crate::http::Response) -> Option<Value> {
    (response.status == 200)
        .then(|| serde_json::from_slice(&response.body).ok())
        .flatten()
}

fn fetch_models(config: &LocalServerConfig) -> Vec<String> {
    local_request(config, "/v1/models")
        .ok()
        .and_then(response_json)
        .map(|value| {
            value["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|model| model["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

pub fn detect_prometheus_type(text: &str) -> Option<LocalServerType> {
    if text
        .lines()
        .any(|line| line.trim_start().starts_with("sglang:"))
    {
        Some(LocalServerType::Sglang)
    } else if text
        .lines()
        .any(|line| line.trim_start().starts_with("vllm:"))
    {
        Some(LocalServerType::Vllm)
    } else if text
        .lines()
        .any(|line| line.trim_start().starts_with("llamacpp:"))
    {
        Some(LocalServerType::Llamacpp)
    } else {
        None
    }
}

pub fn fetch_local_server(
    config: &LocalServerConfig,
    now: i64,
    detected: &mut Option<LocalServerType>,
    counters: &mut CounterState,
) -> SparkSnapshot {
    let requested = detected.unwrap_or(config.server_type);
    let result = (|| -> Result<(SparkSnapshot, LocalServerType), String> {
        if matches!(
            requested,
            LocalServerType::Auto | LocalServerType::SparkDash
        ) {
            if let Ok(response) = local_request(config, "/api/stats") {
                if let Some(value) = response_json(response) {
                    if value["endpoints"].is_array() {
                        let mut snapshot = parse_spark(&value, now);
                        snapshot.name = config.name.clone();
                        return Ok((snapshot, LocalServerType::SparkDash));
                    }
                }
            }
            if requested == LocalServerType::SparkDash {
                return Err("spark-dash /api/stats unavailable".into());
            }
        }
        if matches!(
            requested,
            LocalServerType::Auto
                | LocalServerType::Sglang
                | LocalServerType::Vllm
                | LocalServerType::Llamacpp
        ) {
            if let Ok(response) = local_request(config, "/metrics") {
                if response.status == 200 {
                    let text = String::from_utf8_lossy(&response.body);
                    let engine = if requested == LocalServerType::Auto {
                        detect_prometheus_type(&text)
                    } else {
                        Some(requested)
                    };
                    if let Some(engine) = engine {
                        return Ok((
                            parse_prometheus(
                                &config.name,
                                engine,
                                &text,
                                fetch_models(config),
                                now,
                                counters,
                            ),
                            engine,
                        ));
                    }
                }
            }
            if requested != LocalServerType::Auto {
                return Err(format!("{} /metrics unavailable", requested.as_str()));
            }
        }
        if matches!(requested, LocalServerType::Auto | LocalServerType::Ollama) {
            let version = local_request(config, "/api/version").ok();
            if version.is_some_and(|response| response.status == 200) {
                if let Some(value) = local_request(config, "/api/ps")
                    .ok()
                    .and_then(response_json)
                {
                    return Ok((
                        parse_ollama(&config.name, &value, now),
                        LocalServerType::Ollama,
                    ));
                }
            }
            if requested == LocalServerType::Ollama {
                return Err("ollama API unavailable".into());
            }
        }
        if matches!(requested, LocalServerType::Auto | LocalServerType::Lmstudio) {
            if let Some(value) = local_request(config, "/v1/models")
                .ok()
                .and_then(response_json)
            {
                return Ok((
                    parse_models(&config.name, LocalServerType::Lmstudio, &value, now),
                    LocalServerType::Lmstudio,
                ));
            }
        }
        Err("No supported local AI API detected".into())
    })();
    match result {
        Ok((snapshot, engine)) => {
            *detected = Some(engine);
            snapshot
        }
        Err(error) => {
            if config.server_type == LocalServerType::Auto && detected.take().is_some() {
                return fetch_local_server(config, now, detected, counters);
            }
            SparkSnapshot {
                name: config.name.clone(),
                engine: detected.unwrap_or(config.server_type).as_str().to_owned(),
                fetched_at: now,
                error: Some(error),
                ..Default::default()
            }
        }
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
        "reset_since_last_run": window.reset_since_last_run,
        "card_only": window.card_only
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
    fn live_codex_plus_weekly_only() {
        let reading = parse_codex_usage(
            &json!({
                "plan_type": "plus",
                "rate_limit": {
                    "primary_window": null,
                    "secondary_window": {
                        "used_percent": 5.0,
                        "reset_at": 2000,
                        "limit_window_seconds": 604800
                    }
                }
            }),
            1000,
        );
        assert_eq!(reading.plan_type.as_deref(), Some("plus"));
        assert_eq!(reading.windows.len(), 1);
        assert_eq!(reading.windows[0].key, "week");
        assert!(reading.live);
    }

    #[test]
    fn live_codex_pro_both_windows_and_additional_limit() {
        let reading = parse_codex_usage(
            &json!({
                "plan_type": "pro",
                "rate_limit": {
                    "primary_window": {"used_percent":15.0,"reset_at":2000,"limit_window_seconds":18000},
                    "secondary_window": {"used_percent":5.0,"reset_at":3000,"limit_window_seconds":604800}
                },
                "additional_rate_limits": [{
                    "name": "GPT-5",
                    "primary_window": {"used_percent":7.0,"reset_at":4000,"limit_window_seconds":2592000}
                }]
            }),
            1000,
        );
        assert_eq!(reading.windows.len(), 3);
        assert_eq!(reading.windows[0].key, "5h");
        assert_eq!(reading.windows[1].key, "week");
        assert_eq!(reading.windows[2].key, "month");
        assert_eq!(reading.windows[2].label, "GPT-5");
        assert!(reading.windows[2].card_only);
    }

    #[test]
    fn live_codex_nulls_and_missing_fields_are_skipped() {
        let reading = parse_codex_usage(
            &json!({
                "rate_limit": {"primary_window": null},
                "additional_rate_limits": [
                    {"name":"missing window"},
                    {"primary_window":{"used_percent":1,"limit_window_seconds":18000}}
                ]
            }),
            1000,
        );
        assert!(reading.windows.is_empty());
        assert_eq!(reading.plan_type, None);
    }

    #[test]
    fn live_window_keys_follow_duration() {
        assert_eq!(window_key(18_000), "5h");
        assert_eq!(window_key(604_800), "week");
        assert_eq!(window_key(2_592_000), "month");
        assert_eq!(window_key(86_400), "1d");
        assert_eq!(window_key(10_800), "3h");
    }

    #[test]
    fn jwt_exp_decodes_padded_and_paddingless_payloads() {
        let payload = "eyJleHAiOjIwMDAwMDAwMDB9";
        assert_eq!(
            jwt_exp(&format!("header.{payload}.signature")),
            Some(2_000_000_000)
        );
        assert_eq!(
            jwt_exp(&format!("header.{payload}==.signature")),
            Some(2_000_000_000)
        );
        assert_eq!(jwt_exp("header.!bad.signature"), None);
        assert_eq!(jwt_exp("header.e30.signature"), None);
    }

    #[test]
    fn auth_json_requires_chatgpt_mode_and_both_fields() {
        let valid = parse_codex_auth(
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"fake-token","account_id":"00000000-0000-0000-0000-000000000000"}}"#,
        )
        .unwrap();
        assert_eq!(valid.access_token, "fake-token");
        assert!(parse_codex_auth(
            r#"{"auth_mode":"apikey","tokens":{"access_token":"fake-token","account_id":"00000000-0000-0000-0000-000000000000"}}"#
        )
        .is_none());
        assert!(parse_codex_auth(r#"{"auth_mode":"chatgpt","tokens":{}}"#).is_none());
    }

    #[test]
    fn config_base_url_uses_only_active_assignment() {
        assert_eq!(
            codex_usage_url_from_config(Some(
                "chatgpt_base_url = \"https://example.test/backend-api\""
            )),
            "https://example.test/backend-api/wham/usage"
        );
        assert_eq!(
            codex_usage_url_from_config(Some("# chatgpt_base_url = \"https://ignored.test\"")),
            "https://chatgpt.com/backend-api/wham/usage"
        );
        assert_eq!(
            codex_usage_url_from_config(None),
            "https://chatgpt.com/backend-api/wham/usage"
        );
        assert_eq!(
            codex_usage_url_from_config(Some("chatgpt_base_url = \"https://example.test/custom\"")),
            "https://example.test/custom/api/codex/usage"
        );
    }

    #[test]
    fn spark_missing_fields() {
        let spark = parse_spark(&json!({"endpoints":[{"up":true,"tok_s":2.5}]}), 1);
        assert_eq!(spark.tok_s, 2.5);
        assert_eq!(spark.gpu_util, 0.0);
    }

    #[test]
    fn local_server_lines_and_migration() {
        let servers = parse_local_servers(
            "# lab\nFast http://gpu-box.local:8000 sglang\nChat|http://10.0.0.5:11434|ollama",
        )
        .unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].server_type, LocalServerType::Sglang);
        assert_eq!(servers[1].name, "Chat");
        assert!(parse_local_servers("bad https://gpu-box.local:8000 auto").is_err());
        assert_eq!(
            migrate_spark_host(None, Some("gpu-box.local:8000")).as_deref(),
            Some("Spark|http://gpu-box.local:8000|auto")
        );
        assert_eq!(
            migrate_spark_host(
                Some("Lab|http://gpu-box.local:8000|auto"),
                Some("10.0.0.5:11434")
            ),
            None
        );
    }

    #[test]
    fn prometheus_sums_label_sets_and_maps_sglang() {
        let text = r#"
# HELP ignored
sglang:num_running_reqs{model="a"} 1
sglang:num_running_reqs{model="b"} 2
sglang:num_queue_reqs 4
sglang:gen_throughput 126.5
sglang:token_usage 0.42
"#;
        let snapshot = parse_prometheus(
            "Fast",
            LocalServerType::Sglang,
            text,
            vec!["invented/model".into()],
            10,
            &mut CounterState::default(),
        );
        assert_eq!(snapshot.endpoints[0]["running"], 3);
        assert_eq!(snapshot.endpoints[0]["waiting"], 4);
        assert_eq!(snapshot.tok_s, 126.5);
        assert_eq!(snapshot.endpoints[0]["kv_pct"], 42.0);
    }

    #[test]
    fn vllm_counter_reset_and_ttft_delta() {
        let mut counters = CounterState::default();
        let first = "vllm:num_requests_running 0\nvllm:num_requests_waiting 1\nvllm:generation_tokens_total 100\nvllm:gpu_cache_usage_perc 0.25\nvllm:time_to_first_token_seconds_sum 2\nvllm:time_to_first_token_seconds_count 4";
        let second = "vllm:num_requests_running 2\nvllm:num_requests_waiting 0\nvllm:generation_tokens_total 160\nvllm:gpu_cache_usage_perc 0.5\nvllm:time_to_first_token_seconds_sum 3.2\nvllm:time_to_first_token_seconds_count 6";
        assert_eq!(
            parse_prometheus("V", LocalServerType::Vllm, first, vec![], 10, &mut counters).tok_s,
            0.0
        );
        let snapshot = parse_prometheus(
            "V",
            LocalServerType::Vllm,
            second,
            vec![],
            20,
            &mut counters,
        );
        assert_eq!(snapshot.tok_s, 6.0);
        assert!((snapshot.endpoints[0]["ttft_ms"].as_f64().unwrap() - 600.0).abs() < 0.001);
        let reset = parse_prometheus(
            "V",
            LocalServerType::Vllm,
            "vllm:generation_tokens_total 3",
            vec![],
            30,
            &mut counters,
        );
        assert_eq!(reset.tok_s, 0.0);
    }

    #[test]
    fn llamacpp_mapping_and_ollama_ps() {
        let snapshot = parse_prometheus(
            "Cpp",
            LocalServerType::Llamacpp,
            "llamacpp:requests_processing 1\nllamacpp:requests_deferred 2\nllamacpp:kv_cache_usage_ratio 0.75",
            vec![],
            1,
            &mut CounterState::default(),
        );
        assert_eq!(snapshot.endpoints[0]["running"], 1);
        assert_eq!(snapshot.endpoints[0]["waiting"], 2);
        assert_eq!(snapshot.endpoints[0]["kv_pct"], 75.0);
        let ollama = parse_ollama(
            "Chat",
            &json!({"models":[{"name":"invented/a","size_vram":100},{"name":"invented/b","size_vram":250}]}),
            2,
        );
        assert_eq!(ollama.endpoints[0]["models"].as_array().unwrap().len(), 2);
        assert_eq!(ollama.host["memory_bytes"], 350);
        assert!(!ollama.busy);
    }

    #[test]
    fn auto_detection_order_helpers() {
        assert_eq!(
            detect_prometheus_type("vllm:x 1\nsglang:y 2"),
            Some(LocalServerType::Sglang)
        );
        assert_eq!(
            detect_prometheus_type("vllm:x 1\nllamacpp:y 2"),
            Some(LocalServerType::Vllm)
        );
        assert_eq!(
            detect_prometheus_type("llamacpp:x 1"),
            Some(LocalServerType::Llamacpp)
        );
    }
}
