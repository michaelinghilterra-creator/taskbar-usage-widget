use serde_json::{json, Value};
use std::fs;
use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
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
                if matches!(name, Some("get_usage_headroom" | "get_spark_status")) {
                    let headroom = usage_headroom(now());
                    let result = if name == Some("get_spark_status") {
                        headroom["spark"].clone()
                    } else {
                        headroom
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
            "description": "Compact local Spark inference status",
            "inputSchema": schema
        }
    ])
}

fn usage_headroom(time: i64) -> Value {
    let path = crate::storage::dir().join("state.json");
    let state = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let fresh = state
        .as_ref()
        .and_then(|value| value["written_at"].as_i64())
        .is_some_and(|written| time - written <= 600);
    let mut readings = state
        .as_ref()
        .and_then(|value| value["readings"].as_array())
        .cloned()
        .unwrap_or_default();
    let mut spark = state.as_ref().and_then(|value| value.get("spark")).cloned();

    if !fresh {
        readings.retain(|reading| reading["provider"].as_str() == Some("claude"));
        for reading in &mut readings {
            reading["error"] = json!("unavailable: widget state is stale");
        }
        readings.push(reading_json(&crate::providers::read_codex(time), time));
        if let Some(host) = spark_host() {
            spark = Some(spark_json(&crate::providers::fetch_spark(&host, time)));
        }
    }

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

    json!({
        "providers": providers,
        "spark": spark,
        "summary": summary.join("; "),
        "advice": if claude_tight { "claude_tight" } else { "ok" }
    })
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
        "up": value["up"].as_bool().unwrap_or(false),
        "model": first.map_or(Value::Null, |endpoint| endpoint["model"].clone()),
        "engine": first.map_or(Value::Null, |endpoint| endpoint["engine"].clone()),
        "tok_s": value["tok_s"].as_f64().unwrap_or(0.0),
        "peak_20m": spark_peak_20m(time).max(value["tok_s"].as_f64().unwrap_or(0.0)),
        "running": sum("running"),
        "waiting": sum("waiting"),
        "kv_pct": first.map_or(Value::Null, |endpoint| endpoint["kv_pct"].clone()),
        "ttft_ms": first.map_or(Value::Null, |endpoint| {
            endpoint.get("ttft_ms").or_else(|| endpoint.get("ttft")).cloned().unwrap_or(Value::Null)
        }),
        "gpu_util": value["gpu_util"].as_f64().unwrap_or_else(|| value["sys"]["gpu"]["util"].as_f64().unwrap_or(0.0)),
        "gpu_temp": value["sys"]["gpu"]["temp"],
        "gpu_power": value["sys"]["gpu"]["power"],
        "mem_pct": value["sys"]["memory"]["pct"],
        "busy": value["busy"].as_bool().unwrap_or(false),
        "aux": aux,
        "stale_secs": (time - value["fetched_at"].as_i64().unwrap_or(0)).max(0)
    })
}

fn spark_peak_20m(time: i64) -> f64 {
    let path = crate::storage::dir().join("history.jsonl");
    fs::read_to_string(path)
        .ok()
        .into_iter()
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(|entry| {
            entry["p"].as_str() == Some("spark")
                && entry["t"].as_i64().is_some_and(|at| at >= time - 1200)
        })
        .filter_map(|entry| entry["tok"].as_f64())
        .fold(0.0, f64::max)
}

fn spark_host() -> Option<String> {
    let key = crate::http::wide("Software\\TaskbarUsageWidget");
    let name = crate::http::wide("SparkHost");
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
        let end = value
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(value.len());
        let host = String::from_utf16_lossy(&value[..end]);
        (!host.is_empty()).then_some(host)
    }
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
