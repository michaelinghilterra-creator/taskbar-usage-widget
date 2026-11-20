use serde_json::{json, Value};
use std::fs;
use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
#[cfg(not(test))]
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

const PROTOCOL: &str = "2025-06-18";
const SUPPORTED: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

pub fn run() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let _ = serve(stdin.lock(), stdout.lock());
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn serve<R: BufRead, W: Write>(input: R, mut output: W) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            write_json(&mut output, &error(Value::Null, -32700, "Parse error"))?;
            continue;
        };
        if request.get("id").is_none() {
            continue;
        }
        let id = request["id"].clone();
        let response = match request["method"].as_str() {
            Some("initialize") => {
                let requested = request["params"]["protocolVersion"]
                    .as_str()
                    .unwrap_or(PROTOCOL);
                let protocol = if SUPPORTED.contains(&requested) {
                    requested
                } else {
                    PROTOCOL
                };
                ok(
                    id,
                    json!({
                        "protocolVersion": protocol,
                        "capabilities": {"tools": {}},
                        "serverInfo": {
                            "name": "taskbar-usage-widget",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }),
                )
            }
            Some("ping") => ok(id, json!({})),
            Some("tools/list") => ok(id, json!({"tools": tools()})),
            Some("tools/call") => {
                let name = request["params"]["name"].as_str();
                if matches!(
                    name,
                    Some(
                        "get_usage_headroom"
                            | "get_spark_status"
                            | "get_local_servers"
                            | "recommend_worker"
                    )
                ) {
                    let mut headroom = usage_headroom(now());
                    // Text below comes from servers, not from the user: strip
                    // control characters everywhere and keep server-reported
                    // names short so they cannot carry instructions.
                    clean_strings(&mut headroom, 300);
                    clean_strings(&mut headroom["spark"], 80);
                    clean_strings(&mut headroom["local_servers"], 80);
                    let result = match name {
                        Some("get_spark_status") => headroom["spark"].clone(),
                        Some("get_local_servers") => headroom["local_servers"].clone(),
                        Some("recommend_worker") => {
                            let task_size = request["params"]["arguments"]["task_size"]
                                .as_str()
                                .unwrap_or("medium");
                            let router = &headroom["router"];
                            let bulk = router["recommendation"].as_str().unwrap_or("conserve");
                            let claude = crate::router::ProviderStatus::from_str(
                                router["claude"].as_str().unwrap_or("unknown"),
                            );
                            let worker = crate::router::recommend_worker(task_size, claude, bulk);
                            json!({
                                "worker": worker,
                                "reason": router["reason"],
                                "claude": router["claude"],
                                "codex": router["codex"],
                                "spark": router["spark"]
                            })
                        }
                        _ => headroom,
                    };
                    ok(
                        id,
                        json!({"content": [{"type": "text", "text": result.to_string()}]}),
                    )
                } else {
                    error(id, -32602, "Unknown tool")
                }
            }
            _ => error(id, -32601, "Method not found"),
        };
        write_json(&mut output, &response)?;
    }
    Ok(())
}

/// Replaces control characters with spaces and truncates the text.
pub fn clean_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max_chars)
        .collect()
}

fn clean_strings(value: &mut Value, max_chars: usize) {
    match value {
        Value::String(text) => *text = clean_text(text, max_chars),
        Value::Array(items) => items.iter_mut().for_each(|v| clean_strings(v, max_chars)),
        Value::Object(map) => map.values_mut().for_each(|v| clean_strings(v, max_chars)),
        _ => {}
    }
}

fn write_json(output: &mut impl Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn tools() -> Value {
    let schema = json!({"type": "object", "properties": {}, "additionalProperties": false});
    json!([
        {
            "name": "get_usage_headroom",
            "description": "Current Claude and Codex quota headroom and local Spark status",
            "inputSchema": schema.clone()
        },
        {
            "name": "get_spark_status",
            "description": "Compatibility view of the first local AI server",
            "inputSchema": schema.clone()
        },
        {
            "name": "get_local_servers",
            "description": "Compact status for every configured local AI server",
            "inputSchema": schema
        },
        {
            "name": "recommend_worker",
            "description": "Recommend a worker for a small, medium, or large task",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_size": {
                        "type": "string",
                        "enum": ["small", "medium", "large"],
                        "default": "medium"
                    }
                },
                "additionalProperties": false
            }
        }
    ])
}

pub fn load_cached_state() -> Option<Value> {
    fs::read(crate::storage::dir().join("state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
}

fn usage_headroom(time: i64) -> Value {
    #[cfg(test)]
    let state = Some(json!({
        "written_at": time,
        "readings": [{"provider":"claude","as_of":time,"windows":[]}],
        "spark": null
    }));
    #[cfg(not(test))]
    let state = load_cached_state();
    let fresh = state
        .as_ref()
        .and_then(|value| value["written_at"].as_i64())
        .is_some_and(|written| time - written <= 600);
    let mut readings = state
        .as_ref()
        .and_then(|value| value["readings"].as_array())
        .cloned()
        .unwrap_or_default();
    let mut local_servers = state
        .as_ref()
        .and_then(|value| value["local_servers"].as_array())
        .cloned()
        .unwrap_or_default();
    let mut spark = state.as_ref().and_then(|value| value.get("spark")).cloned();

    if !fresh {
        readings.retain(|reading| reading["provider"].as_str() == Some("claude"));
        for reading in &mut readings {
            reading["error"] = json!("unavailable: widget state is stale");
        }
        readings.push(reading_json(&crate::providers::read_codex(time), time));
        if let Some(config_text) = read_registry_string("LocalServers") {
            if let Ok(configs) = crate::providers::parse_local_servers(&config_text) {
                local_servers = configs
                    .iter()
                    .map(|config| {
                        let mut detected = None;
                        let mut counters = Default::default();
                        spark_json(&crate::providers::fetch_local_server(
                            config,
                            time,
                            &mut detected,
                            &mut counters,
                        ))
                    })
                    .collect();
                spark = local_servers.first().cloned();
            }
        }
    }

    let router_state =
        json!({"readings": readings, "spark": spark, "local_servers": local_servers});
    let router = crate::router::verdict(&router_state, time);
    let readings = router_state["readings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let spark = router_state.get("spark").cloned();
    let local_servers = router_state["local_servers"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let mut claude_number = 0;
    let mut providers = Vec::new();
    let mut summary = Vec::new();
    let mut claude_tight = false;
    for mut reading in readings {
        let provider = reading["provider"].as_str().unwrap_or("unknown").to_owned();
        if provider == "claude" {
            claude_number += 1;
            // Never expose email addresses or folder paths through MCP.
            reading["account"] = json!(format!("account {claude_number}"));
        } else {
            reading
                .as_object_mut()
                .map(|object| object.remove("account"));
        }
        let stale = time - reading["as_of"].as_i64().unwrap_or(0);
        let mut windows = Vec::new();
        if let Some(source) = reading["windows"].as_array() {
            for window in source {
                let normalized = normalize_window(window, time, stale);
                let used = normalized["used_pct"].as_f64().unwrap_or(0.0);
                let projected = normalized["projected_at_reset_pct"].as_f64().unwrap_or(0.0);
                let full = normalized["full_at"].is_string();
                if provider == "claude" && (used >= 90.0 || projected >= 100.0 || full) {
                    claude_tight = true;
                }
                summary.push(format!(
                    "{} {} {:.0}%",
                    title(&provider),
                    normalized["key"].as_str().unwrap_or("window"),
                    used
                ));
                windows.push(normalized);
            }
        }
        reading["windows"] = Value::Array(windows);
        providers.push(reading);
    }

    let spark = spark.filter(|value| !value.is_null()).map(|value| {
        let compact = compact_spark(&value, time);
        summary.push(if compact["up"].as_bool().unwrap_or(false) {
            format!(
                "Spark up, {} running",
                compact["running"].as_i64().unwrap_or(0)
            )
        } else {
            "Spark down".into()
        });
        compact
    });
    let local_servers = local_servers
        .iter()
        .map(|value| compact_spark(value, time))
        .collect::<Vec<_>>();

    json!({
        "providers": providers,
        "spark": spark,
        "local_servers": local_servers,
        "router": router.json(),
        "summary": summary.join("; "),
        "advice": if claude_tight { "claude_tight" } else { "ok" },
        "value": value_json(time)
    })
}

fn value_json(time: i64) -> Value {
    #[cfg(test)]
    {
        let _ = time;
        return json!({
        "claude":{"today_usd":12.0,"week_usd":184.0,"month_usd":640.0,"plan_multiple":null,"top_projects_today":[{"name":"alpha","share":1.0}],"unpriced":[]},
        "codex":{"today_usd":4.2,"week_usd":58.0,"month_usd":126.0,"plan_multiple":null,"top_projects_today":[{"name":"beta","share":1.0}],"unpriced":[]},
        "pricing_as_of":"2026-11-16",
        "partial":true
        });
    }
    #[cfg(not(test))]
    {
        let pricing = crate::value::Pricing::load();
        let snapshot =
            crate::value::Store::load().snapshot(crate::tokens::local_day(time), &pricing);
        let provider = |value: &crate::value::ProviderValue, setting: &str| {
            let plan = read_registry_string(setting)
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|v| *v > 0.0);
            json!({
                "today_usd": value.today_usd,
                "week_usd": value.week_usd,
                "month_usd": value.month_usd,
                "plan_multiple": plan.map(|p| value.month_usd / p),
                "top_projects_today": value.top_projects_today.iter().map(|project| json!({"name":project.name,"share":project.share})).collect::<Vec<_>>(),
                "unpriced": value.unpriced
            })
        };
        json!({
            "claude": provider(&snapshot.claude, "ClaudePlanUsd"),
            "codex": provider(&snapshot.codex, "CodexPlanUsd"),
            "pricing_as_of": snapshot.pricing_as_of,
            "partial": snapshot.claude.calculating || snapshot.codex.calculating
        })
    }
}

#[cfg(not(test))]
fn read_registry_string(setting: &str) -> Option<String> {
    let key = crate::http::wide("Software\\TaskbarUsageWidget");
    let name = crate::http::wide(setting);
    let mut size = 0;
    unsafe {
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        ) != 0
        {
            return None;
        }
        let mut value = vec![0u16; size as usize / 2 + 1];
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            value.as_mut_ptr().cast(),
            &mut size,
        ) != 0
        {
            return None;
        }
        let end = value.iter().position(|c| *c == 0).unwrap_or(value.len());
        Some(String::from_utf16_lossy(&value[..end]))
    }
}

#[cfg(test)]
fn read_registry_string(_: &str) -> Option<String> {
    None
}

fn normalize_window(window: &Value, time: i64, stale: i64) -> Value {
    let used = window["percent"].as_f64().unwrap_or(0.0);
    let elapsed = window["elapsed_pct"].as_f64().unwrap_or(0.0);
    let projected = window["projected"].as_f64().unwrap_or(used);
    let reset = window["resets_at"].as_i64();
    let full = window["full_at"].as_i64();
    json!({
        "key": window["key"],
        "label": window["label"],
        "used_pct": used,
        "elapsed_pct": elapsed,
        "ahead_of_pace_by": window["ahead_by"].as_f64().unwrap_or(used - elapsed),
        "projected_at_reset_pct": projected,
        "full_at": full.map(iso_local),
        "resets_at": reset.map(iso_local),
        "resets_in": reset.map(|at| crate::providers::resets_in(at - time)),
        "stale_secs": stale.max(0)
    })
}

fn reading_json(reading: &crate::providers::Reading, time: i64) -> Value {
    json!({
        "provider": "codex",
        "fetched_at": reading.fetched_at,
        "as_of": reading.as_of,
        "error": reading.error,
        "plan_type": reading.plan_type,
        "windows": reading.windows.iter().map(|window| {
            crate::providers::window_json(window, time, None)
        }).collect::<Vec<_>>()
    })
}

fn spark_json(spark: &crate::providers::SparkSnapshot) -> Value {
    json!({
        "name": spark.name,
        "type": spark.engine,
        "fetched_at": spark.fetched_at,
        "up": spark.up,
        "model": spark.endpoints.first().and_then(|e| e["model"].as_str()),
        "tok_s": spark.tok_s,
        "busy": spark.busy,
        "kv_pct": spark.endpoints.first().and_then(|e| e["kv_pct"].as_f64()),
        "gpu_util": spark.gpu_util,
        "gpu_temp": spark.sys["gpu"]["temp"],
        "sys": spark.sys,
        "aux": spark.aux,
        "endpoints": spark.endpoints,
        "error": spark.error
    })
}

fn compact_spark(value: &Value, time: i64) -> Value {
    let endpoints = value["endpoints"].as_array().cloned().unwrap_or_default();
    let first = endpoints.first();
    let sum = |field: &str| {
        endpoints
            .iter()
            .filter_map(|endpoint| endpoint[field].as_i64())
            .sum::<i64>()
    };
    let aux = value["aux"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|service| {
            json!({
                "name": service["name"],
                "up": service["up"].as_bool().unwrap_or(false),
                "busy": service["busy"].as_bool().unwrap_or(false)
            })
        })
        .collect::<Vec<_>>();
    json!({
        "name": value["name"],
        "type": value["type"],
        "up": value["up"].as_bool().unwrap_or(false),
        "model": first.map_or(Value::Null, |endpoint| endpoint["model"].clone()),
        "engine": first.map_or(Value::Null, |endpoint| endpoint["engine"].clone()),
        "tok_s": value["tok_s"].as_f64().unwrap_or(0.0),
        "peak_20m": server_peak_20m(time, value["name"].as_str().unwrap_or_default())
            .max(value["tok_s"].as_f64().unwrap_or(0.0)),
        "running": value["running"].as_i64().unwrap_or_else(|| sum("running")),
        "waiting": value["waiting"].as_i64().unwrap_or_else(|| sum("waiting")),
        "kv_pct": first.map_or(Value::Null, |endpoint| endpoint["kv_pct"].clone()),
        "ttft_ms": first.map_or(Value::Null, |endpoint| {
            endpoint.get("ttft_ms").or_else(|| endpoint.get("ttft")).cloned().unwrap_or(Value::Null)
        }),
        "gpu_util": value["gpu_util"].as_f64().unwrap_or_else(|| value["sys"]["gpu"]["util"].as_f64().unwrap_or(0.0)),
        "gpu_temp": value["sys"]["gpu"]["temp"],
        "gpu_power": value["sys"]["gpu"]["power"],
        "mem_pct": value["host"]["mem_pct"]
            .as_f64()
            .or_else(|| value["sys"]["memory"]["pct"].as_f64()),
        "busy": value["busy"].as_bool().unwrap_or(false),
        "aux": aux,
        "stale_secs": (time - value["fetched_at"].as_i64().unwrap_or(0)).max(0)
    })
}

/// Highest tok/s in the last 20 minutes for one local server, from its own
/// `local:<name>` history lines.
fn server_peak_20m(time: i64, name: &str) -> f64 {
    let key = format!("local:{name}");
    let path = crate::storage::dir().join("history.jsonl");
    fs::read_to_string(path)
        .ok()
        .into_iter()
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(|entry| {
            entry["p"].as_str() == Some(key.as_str())
                && entry["t"].as_i64().is_some_and(|at| at >= time - 1200)
        })
        .filter_map(|entry| entry["tok"].as_f64())
        .fold(0.0, f64::max)
}

fn iso_local(unix: i64) -> String {
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
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
        )
    }
}

fn title(provider: &str) -> &str {
    match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        "spark" => "Spark",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_text_is_cleaned_and_capped() {
        let long = format!("m\n\u{1b}[0m{}", "x".repeat(500));
        let mut value = json!({"local_servers":[{"model":long,"running":2}]});
        clean_strings(&mut value["local_servers"], 80);
        let model = value["local_servers"][0]["model"].as_str().unwrap();
        assert_eq!(model.chars().count(), 80);
        assert!(!model.chars().any(char::is_control));
        assert_eq!(value["local_servers"][0]["running"], 2);
    }
    use std::io::Cursor;

    fn exchange(lines: &str) -> Vec<Value> {
        let mut output = Vec::new();
        serve(Cursor::new(lines.as_bytes()), &mut output).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn initialize_round_trip() {
        let result = exchange(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}
"#,
        );
        assert_eq!(result[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(
            result[0]["result"]["serverInfo"]["name"],
            "taskbar-usage-widget"
        );
    }

    #[test]
    fn tools_list_round_trip() {
        let result = exchange(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}
"#,
        );
        assert_eq!(
            result[0]["result"]["tools"][0]["name"],
            "get_usage_headroom"
        );
        assert_eq!(result[0]["result"]["tools"][2]["name"], "get_local_servers");
        assert_eq!(result[0]["result"]["tools"][3]["name"], "recommend_worker");
        assert_eq!(
            result[0]["result"]["tools"][3]["inputSchema"]["properties"]["task_size"]["default"],
            "medium"
        );
    }

    #[test]
    fn tools_call_round_trip() {
        let result = exchange(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_usage_headroom","arguments":{}}}
"#,
        );
        let text = result[0]["result"]["content"][0]["text"].as_str().unwrap();
        let body: Value = serde_json::from_str(text).unwrap();
        assert!(body["summary"].is_string());
        assert!(body["advice"].is_string());
        assert!(body["router"]["recommendation"].is_string());
        assert!(body["value"]["claude"]["today_usd"].is_number());
        assert_eq!(body["value"]["partial"], true);
        assert!(
            text.len() < 2_200,
            "MCP headroom response was {} characters",
            text.len()
        );
    }

    #[test]
    fn recommend_worker_round_trip() {
        let result = exchange(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"recommend_worker","arguments":{"task_size":"small"}}}
"#,
        );
        let text = result[0]["result"]["content"][0]["text"].as_str().unwrap();
        let body: Value = serde_json::from_str(text).unwrap();
        assert!(body["worker"].is_string());
        assert!(body["reason"].is_string());
        assert!(body["claude"].is_string());
    }

    #[test]
    fn compact_spark_shape() {
        let source = json!({
            "fetched_at": 900,
            "up": true,
            "tok_s": 84.0,
            "busy": true,
            "gpu_util": 72.0,
            "endpoints": [{
                "model": "model-a", "engine": "engine-a", "running": 2,
                "waiting": 1, "kv_pct": 18.0, "ttft_ms": 43.0
            }],
            "sys": {"gpu": {"temp": 61.0, "power": 210.0}, "memory": {"pct": 48.0}},
            "aux": [{"name": "TTS", "up": true, "busy": false, "private": "drop"}]
        });
        let compact = compact_spark(&source, 1000);
        let object = compact.as_object().unwrap();
        let expected = [
            "name",
            "type",
            "up",
            "model",
            "engine",
            "tok_s",
            "peak_20m",
            "running",
            "waiting",
            "kv_pct",
            "ttft_ms",
            "gpu_util",
            "gpu_temp",
            "gpu_power",
            "mem_pct",
            "busy",
            "aux",
            "stale_secs",
        ];
        assert_eq!(object.len(), expected.len());
        assert!(expected.iter().all(|key| object.contains_key(*key)));
        assert!(object.get("endpoints").is_none());
        assert_eq!(
            compact["aux"][0],
            json!({"name":"TTS","up":true,"busy":false})
        );
    }
}
