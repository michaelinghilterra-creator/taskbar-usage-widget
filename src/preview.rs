use crate::{flyout, providers, render, storage};
use serde_json::{json, Value};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn run(output: &Path) -> Result<Vec<PathBuf>, String> {
    fs::create_dir_all(output).map_err(|error| error.to_string())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let state = fs::read(storage::dir().join("state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .unwrap_or_else(|| fake_state(now));
    let mut spark_points = history_points(now, 1200);
    if spark_points.len() < 3 && state["_preview_fake"].as_bool() == Some(true) {
        spark_points = vec![12.0, 48.0, 31.0, 84.0, 66.0, 102.0, 84.0];
    }
    let mut written = Vec::new();

    for (dark, theme) in [(true, "dark"), (false, "light")] {
        for (scale, scale_name) in [(1.0, "100"), (1.5, "150")] {
            let height = (48.0 * scale) as i32;
            let model = taskbar_model(&state, dark, now, &spark_points);
            let frame = render::render(&model, height, scale, None);
            let background = if dark {
                [0x1c, 0x1c, 0x1c]
            } else {
                [0xf3, 0xf3, 0xf3]
            };
            let pixels = composite_taskbar(&frame, background, height);
            let path = output.join(format!("taskbar-{theme}-{scale_name}.bmp"));
            write_bmp(&path, frame.w + 32, height, &pixels).map_err(|error| error.to_string())?;
            written.push(path);
        }
        let snapshot = flyout_model(&state, now, &spark_points);
        for pinned in [false, true] {
            let (width, height, pixels) = flyout::render(&snapshot, dark, 144, pinned);
            let suffix = if pinned { "-pinned" } else { "" };
            let path = output.join(format!("flyout-{theme}-150{suffix}.bmp"));
            write_bmp_u32(&path, width, height, &pixels).map_err(|error| error.to_string())?;
            written.push(path);
        }
        let wide_width = (560.0_f32 * 1.5).round() as i32;
        let (width, height, pixels) =
            flyout::render_at_width(&snapshot, dark, 144, false, wide_width);
        let path = output.join(format!("flyout-{theme}-150-wide.bmp"));
        write_bmp_u32(&path, width, height, &pixels).map_err(|error| error.to_string())?;
        written.push(path);

        let path = output.join(format!("flyout-{theme}-150-states.bmp"));
        write_flyout_states(&path, dark, now, &spark_points).map_err(|error| error.to_string())?;
        written.push(path);
    }
    let states_path = output.join("taskbar-dark-150-states.bmp");
    write_taskbar_states(&states_path, &state, now, &spark_points)
        .map_err(|error| error.to_string())?;
    written.push(states_path);
    Ok(written)
}

fn write_flyout_states(path: &Path, dark: bool, now: i64, spark_points: &[f32]) -> io::Result<()> {
    let state = fake_state(now);
    let mut mixed = flyout_model(&state, now, spark_points);
    for section in &mut mixed.sections {
        match section.provider {
            flyout::ProviderKind::Claude => {
                if let Some(bar) = section.bars.first_mut() {
                    bar.percent = 68.0;
                    bar.elapsed = 40.0;
                    bar.projected = 92.0;
                    bar.ahead_by = 28.0;
                    bar.full_at = None;
                }
            }
            flyout::ProviderKind::Codex => {
                section.stale = true;
                section.as_of = "6 h old".into();
            }
            flyout::ProviderKind::Spark => {
                section.error = Some("Down".into());
            }
        }
    }
    let empty = flyout::Snapshot {
        updated: "Loading".into(),
        sections: vec![
            flyout::Section {
                provider: flyout::ProviderKind::Claude,
                title: "Claude".into(),
                chip: "Loading".into(),
                ..Default::default()
            },
            flyout::Section {
                provider: flyout::ProviderKind::Codex,
                title: "Codex · No readings yet".into(),
                ..Default::default()
            },
        ],
    };
    let cards = [
        flyout::render(&mixed, dark, 144, false),
        flyout::render(&empty, dark, 144, false),
    ];
    let gap = 18;
    let width = cards.iter().map(|card| card.0).max().unwrap_or(1);
    let height = cards.iter().map(|card| card.1).sum::<i32>() + gap;
    let background = if dark { 0xff1c1c1c } else { 0xfff3f3f3 };
    let mut pixels = vec![background; (width * height) as usize];
    let mut top = 0;
    for (card_width, card_height, card_pixels) in cards {
        for y in 0..card_height {
            for x in 0..card_width {
                pixels[((top + y) * width + x) as usize] =
                    card_pixels[(y * card_width + x) as usize];
            }
        }
        top += card_height + gap;
    }
    write_bmp_u32(path, width, height, &pixels)
}

fn write_taskbar_states(
    path: &Path,
    state: &Value,
    now: i64,
    spark_points: &[f32],
) -> io::Result<()> {
    let scale = 1.5;
    let height = (48.0 * scale) as i32;
    let base = taskbar_model(state, true, now, spark_points);
    let mut models = Vec::new();
    for kind in 0..6 {
        let mut model = base.clone();
        model.show_codex = false;
        model.show_spark = false;
        model.rows.truncate(1);
        let row = model.rows.first_mut().expect("fake preview has Claude");
        row.five = Some(match kind {
            1 => 68.0,
            2 => 98.0,
            _ => 32.0,
        });
        row.week = Some(25.0);
        row.fable = None;
        row.pace = [Some(if kind == 1 { 0.40 } else { 0.55 }), Some(0.60), None];
        row.projected = [Some(if kind == 2 { 100.0 } else { 72.0 }), Some(42.0), None];
        row.stale = kind == 3;
        row.loading = kind == 4;
        row.pulse_alpha = 1.0;
        if kind == 5 {
            row.five = None;
            row.week = None;
        }
        models.push(model);
    }
    for down in [false, true] {
        let mut model = base.clone();
        model.show_claude = false;
        model.show_codex = false;
        model.show_spark = true;
        model.spark_busy = false;
        model.spark_down = down;
        if let Some(row) = &mut model.spark_row {
            row.five = Some(0.0);
            row.critical[0] = down;
        }
        models.push(model);
    }
    let frames = models
        .iter()
        .map(|model| render::render(model, height, scale, None))
        .collect::<Vec<_>>();
    let width = frames.iter().map(|frame| frame.w).max().unwrap_or(1) + 32;
    let total_height = height * frames.len() as i32;
    let mut pixels = vec![[0x1c, 0x1c, 0x1c, 255]; (width * total_height) as usize];
    for (index, frame) in frames.iter().enumerate() {
        let top = index as i32 * height + (height - frame.h) / 2;
        for y in 0..frame.h {
            for x in 0..frame.w {
                let source = frame.px[(y * frame.w + x) as usize];
                let alpha = source[3] as u16;
                let destination = &mut pixels[((top + y) * width + 16 + x) as usize];
                for channel in 0..3 {
                    destination[channel] = (source[channel] as u16
                        + destination[channel] as u16 * (255 - alpha) / 255)
                        .min(255) as u8;
                }
            }
        }
    }
    write_bmp(path, width, total_height, &pixels)
}

fn taskbar_model(state: &Value, dark: bool, now: i64, spark_points: &[f32]) -> render::Model {
    let readings = state["readings"].as_array().cloned().unwrap_or_default();
    let claude = readings
        .iter()
        .filter(|reading| reading["provider"].as_str() == Some("claude"))
        .map(|reading| row_from_windows(&reading["windows"], now))
        .collect::<Vec<_>>();
    let codex_rows = readings
        .iter()
        .find(|reading| reading["provider"].as_str() == Some("codex"))
        .map(|reading| vec![row_from_windows(&reading["windows"], now)])
        .unwrap_or_default();
    let spark = state.get("spark").filter(|value| !value.is_null());
    let spark_row = spark.map(|value| {
        let down = value["error"].is_string() || !value["up"].as_bool().unwrap_or(false);
        let tok_s = value["tok_s"].as_f64().unwrap_or(0.0);
        render::Row {
            five: Some(tok_s),
            week: None,
            fable: None,
            roll: None,
            stale: false,
            pace: [None; 3],
            projected: [None; 3],
            text: [
                Some(if down {
                    "down".into()
                } else if tok_s == 0.0 {
                    "idle".into()
                } else {
                    format!("{tok_s:.0} t/s")
                }),
                None,
                None,
            ],
            critical: [down, false, false],
            loading: false,
            pulse_alpha: 1.0,
        }
    });
    render::Model {
        rows: claude,
        hover: false,
        dark,
        show_left: false,
        font_size: render::DEFAULT_FONT_SIZE,
        hide_icon: false,
        pixel_icon: false,
        icon_size: 18,
        rings: false,
        ring_text: false,
        ring_color_only: false,
        ring_space: false,
        ring_margin: 2,
        hide_labels: false,
        hide_percent: false,
        hide_five: false,
        hide_week: false,
        hide_fable: false,
        rows_per_col: 2,
        provider_mark: 0,
        codex_rows,
        spark_row,
        show_claude: true,
        show_codex: true,
        show_spark: spark.is_some(),
        spark_points: spark_points.to_vec(),
        spark_busy: spark.is_some_and(spark_is_busy),
        spark_down: spark.is_some_and(|value| {
            value["error"].is_string() || !value["up"].as_bool().unwrap_or(false)
        }),
        spark_pulse_alpha: 1.0,
    }
}

fn json_is_active(value: &Value) -> bool {
    value.as_bool().unwrap_or(false)
        || value.as_i64().is_some_and(|count| count > 0)
        || value.as_u64().is_some_and(|count| count > 0)
}

fn service_is_busy(service: &Value) -> bool {
    service["busy"].as_bool().unwrap_or(false)
        || json_is_active(&service["running"])
        || json_is_active(&service["pending"])
}

fn short_service_name(service: &Value) -> String {
    service["name"]
        .as_str()
        .unwrap_or("service")
        .split_whitespace()
        .next()
        .unwrap_or("service")
        .to_owned()
}

fn spark_is_busy(spark: &Value) -> bool {
    let endpoint_busy = spark["endpoints"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|endpoint| json_is_active(&endpoint["running"]));
    endpoint_busy
        || spark["gpu_util"].as_f64().unwrap_or(0.0) > 30.0
        || spark["sys"]["gpu"]["util"].as_f64().unwrap_or(0.0) > 30.0
}

fn row_from_windows(windows: &Value, now: i64) -> render::Row {
    let find = |key: &str| {
        windows.as_array().and_then(|items| {
            items
                .iter()
                .find(|window| window["key"].as_str() == Some(key))
        })
    };
    let data = |window: Option<&Value>| {
        window.map(|value| {
            let percent = value["percent"].as_f64().unwrap_or(0.0);
            let reset = value["resets_at"].as_i64();
            let seconds = value["window_secs"].as_i64();
            let pace = providers::pace(percent, reset, seconds, now, None);
            (percent, pace.elapsed_frac as f32, pace.projected as f32)
        })
    };
    let five = data(find("5h"));
    let week = data(find("week"));
    let fable = data(find("fable"));
    render::Row {
        five: five.map(|value| value.0),
        week: week.map(|value| value.0),
        fable: fable.map(|value| value.0),
        roll: None,
        stale: false,
        pace: [
            five.map(|value| value.1),
            week.map(|value| value.1),
            fable.map(|value| value.1),
        ],
        projected: [
            five.map(|value| value.2),
            week.map(|value| value.2),
            fable.map(|value| value.2),
        ],
        text: [None, None, None],
        critical: [false; 3],
        loading: false,
        pulse_alpha: 1.0,
    }
}

fn flyout_model(state: &Value, now: i64, spark_points: &[f32]) -> flyout::Snapshot {
    let mut sections = Vec::new();
    let readings = state["readings"].as_array().cloned().unwrap_or_default();
    let claude_count = readings
        .iter()
        .filter(|reading| reading["provider"].as_str() == Some("claude"))
        .count();
    let mut claude_number = 0;
    for reading in readings {
        let provider = reading["provider"].as_str().unwrap_or("Unknown");
        let (kind, title, chip) = match provider {
            "claude" => {
                claude_number += 1;
                (
                    flyout::ProviderKind::Claude,
                    "Claude".to_string(),
                    if claude_count > 1 {
                        format!("account {claude_number}")
                    } else {
                        String::new()
                    },
                )
            }
            "codex" => (
                flyout::ProviderKind::Codex,
                "Codex".to_string(),
                title_case(reading["plan_type"].as_str().unwrap_or_default()),
            ),
            _ => continue,
        };
        let mut section = flyout::Section {
            provider: kind,
            title,
            chip,
            error: reading["error"].as_str().map(str::to_owned),
            stale: now - reading["as_of"].as_i64().unwrap_or(now)
                > if provider == "claude" { 600 } else { 21600 },
            ..Default::default()
        };
        for window in reading["windows"].as_array().into_iter().flatten() {
            let percent = window["percent"].as_f64().unwrap_or(0.0);
            let reset = window["resets_at"].as_i64();
            let pace = providers::pace(percent, reset, window["window_secs"].as_i64(), now, None);
            section.bars.push(flyout::Bar {
                label: window["label"].as_str().unwrap_or("Window").to_owned(),
                percent,
                elapsed: pace.elapsed_pct,
                reset: reset
                    .map(|at| crate::local_time(at, at - now > 86400))
                    .unwrap_or_default(),
                ahead_by: pace.ahead_by,
                projected: pace.projected,
                full_at: pace
                    .full_at
                    .map(|at| crate::local_time(at, at - now > 86400)),
            });
        }
        if provider == "claude" {
            section.today = token_text(state, true);
            section.chart = history_values("claude", "5h", now - 86400);
            if section.chart.len() < 3 && state["_preview_fake"].as_bool() == Some(true) {
                section.chart = vec![18.0, 24.0, 31.0, 29.0, 43.0, 51.0, 62.0];
            }
        } else {
            section.today = token_text(state, false);
            section.as_of = format!(
                "as of {} ago",
                short_age(now - reading["as_of"].as_i64().unwrap_or(now))
            );
        }
        sections.push(section);
    }
    if let Some(spark) = state.get("spark").filter(|value| !value.is_null()) {
        let endpoint = spark["endpoints"]
            .as_array()
            .and_then(|items| items.first());
        let mut section = flyout::Section {
            provider: flyout::ProviderKind::Spark,
            title: "Spark".into(),
            chip: endpoint
                .and_then(|value| value["model"].as_str())
                .unwrap_or_default()
                .to_owned(),
            ..Default::default()
        };
        let running: i64 = spark["endpoints"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| value["running"].as_i64())
            .sum();
        let waiting: i64 = spark["endpoints"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| value["waiting"].as_i64())
            .sum();
        let tok_s = spark["tok_s"].as_f64().unwrap_or(0.0);
        section.spark = Some(flyout::SparkStats {
            tok_s,
            peak: spark_points.iter().copied().fold(tok_s as f32, f32::max) as f64,
            running,
            waiting,
            kv_pct: endpoint.and_then(|value| value["kv_pct"].as_f64()),
            ttft_ms: endpoint
                .and_then(|value| value["ttft_ms"].as_f64().or_else(|| value["ttft"].as_f64())),
            gpu_util: spark["gpu_util"].as_f64().unwrap_or(0.0),
            gpu_temp: spark["sys"]["gpu"]["temp"].as_f64(),
            gpu_power: spark["sys"]["gpu"]["power"].as_f64(),
            mem_pct: spark["host"]["mem_pct"]
                .as_f64()
                .or_else(|| spark["sys"]["memory"]["pct"].as_f64()),
            swap_pct: spark["host"]["swap_pct"]
                .as_f64()
                .or_else(|| spark["sys"]["swap"]["pct"].as_f64()),
            aux: spark["aux"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|service| flyout::AuxStatus {
                    name: short_service_name(service),
                    up: service["up"].as_bool().unwrap_or(false),
                    busy: service_is_busy(service),
                })
                .collect(),
        });
        section.chart = spark_points.iter().map(|value| *value as f64).collect();
        section.error = spark["error"].as_str().map(str::to_owned);
        section.stale = now - spark["fetched_at"].as_i64().unwrap_or(now) > 30;
        sections.push(section);
    }
    flyout::Snapshot {
        sections,
        updated: format!(
            "updated {} ago",
            short_age(now - state["written_at"].as_i64().unwrap_or(now))
        ),
    }
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

fn short_age(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds >= 3600 {
        format!("{} h", seconds / 3600)
    } else if seconds >= 60 {
        format!("{} m", seconds / 60)
    } else {
        format!("{} s", seconds)
    }
}

fn compact(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1} M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.0} k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn token_text(state: &Value, claude: bool) -> String {
    if state["tokens"]["checked_at"].as_i64().unwrap_or(0) == 0 {
        return "Today: counting...".into();
    }
    if claude {
        format!(
            "Today: {} tokens · {} out",
            compact(state["tokens"]["claude"].as_u64().unwrap_or(0)),
            compact(state["tokens"]["claude_output"].as_u64().unwrap_or(0))
        )
    } else {
        format!(
            "Today: {} tokens",
            compact(state["tokens"]["codex"].as_u64().unwrap_or(0))
        )
    }
}

fn history_points(now: i64, seconds: i64) -> Vec<f32> {
    history_values("spark", "", now - seconds)
        .into_iter()
        .map(|value| value as f32)
        .collect()
}

fn history_values(provider: &str, key: &str, cutoff: i64) -> Vec<f64> {
    fs::read_to_string(storage::dir().join("history.jsonl"))
        .ok()
        .into_iter()
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(|entry| {
            entry["p"].as_str() == Some(provider)
                && entry["t"].as_i64().is_some_and(|time| time >= cutoff)
                && (provider == "spark" || entry["k"].as_str() == Some(key))
        })
        .filter_map(|entry| {
            if provider == "spark" {
                entry["tok"].as_f64()
            } else {
                entry["v"].as_f64()
            }
        })
        .collect()
}

fn fake_state(now: i64) -> Value {
    let window = |key: &str, label: &str, percent: f64, seconds: i64| {
        json!({
            "key": key, "label": label, "percent": percent,
            "resets_at": now + seconds / 2, "window_secs": seconds
        })
    };
    json!({
        "_preview_fake": true,
        "written_at": now,
        "readings": [
            {"provider":"claude","as_of":now-3600,"windows":[window("5h","5-hour",62.0,18000),window("week","Weekly",38.0,604800)]},
            {"provider":"codex","as_of":now-300,"plan_type":"plus","windows":[window("5h","5-hour",15.0,18000),window("week","Weekly",9.0,604800)]}
        ],
        "spark": {
            "fetched_at": now, "up": true, "tok_s": 84.0, "gpu_util": 67.0, "busy": true,
            "endpoints":[{"model":"local-model","engine":"vllm","running":1,"waiting":0,"kv_pct":22.0,"ttft_ms":41.0}],
            "host":{"mem_pct":46.0,"swap_pct":12.0},
            "sys":{"gpu":{"temp":61.0,"power":205.0}},
            "aux":[
                {"name":"Wan 2.2 T2V A14B","up":true,"running":0,"pending":0},
                {"name":"Chatterbox TTS","up":true,"running":1,"pending":0}
            ]
        },
        "tokens":{"claude":1200000,"claude_output":84000,"codex":238000,"checked_at":now}
    })
}

fn composite_taskbar(frame: &render::Frame, background: [u8; 3], height: i32) -> Vec<[u8; 4]> {
    let width = frame.w + 32;
    let mut output =
        vec![[background[2], background[1], background[0], 255]; (width * height) as usize];
    let left = 16;
    let top = (height - frame.h) / 2;
    for y in 0..frame.h {
        for x in 0..frame.w {
            let source = frame.px[(y * frame.w + x) as usize];
            let alpha = source[3] as u16;
            let destination = &mut output[((top + y) * width + left + x) as usize];
            for channel in 0..3 {
                destination[channel] = (source[channel] as u16
                    + destination[channel] as u16 * (255 - alpha) / 255)
                    .min(255) as u8;
            }
        }
    }
    output
}

fn write_bmp(path: &Path, width: i32, height: i32, pixels: &[[u8; 4]]) -> io::Result<()> {
    let bytes = pixels
        .iter()
        .flat_map(|pixel| pixel.iter().copied())
        .collect::<Vec<_>>();
    write_bmp_bytes(path, width, height, &bytes)
}

fn write_bmp_u32(path: &Path, width: i32, height: i32, pixels: &[u32]) -> io::Result<()> {
    let bytes = pixels
        .iter()
        .flat_map(|pixel| pixel.to_le_bytes())
        .collect::<Vec<_>>();
    write_bmp_bytes(path, width, height, &bytes)
}

fn write_bmp_bytes(path: &Path, width: i32, height: i32, top_down: &[u8]) -> io::Result<()> {
    let image_size = (width * height * 4) as u32;
    let mut output = Vec::with_capacity(54 + image_size as usize);
    output.extend_from_slice(b"BM");
    output.extend_from_slice(&(54 + image_size).to_le_bytes());
    output.extend_from_slice(&[0; 4]);
    output.extend_from_slice(&54u32.to_le_bytes());
    output.extend_from_slice(&40u32.to_le_bytes());
    output.extend_from_slice(&width.to_le_bytes());
    output.extend_from_slice(&height.to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&32u16.to_le_bytes());
    output.extend_from_slice(&0u32.to_le_bytes());
    output.extend_from_slice(&image_size.to_le_bytes());
    output.extend_from_slice(&3780i32.to_le_bytes());
    output.extend_from_slice(&3780i32.to_le_bytes());
    output.extend_from_slice(&0u32.to_le_bytes());
    output.extend_from_slice(&0u32.to_le_bytes());
    let stride = width as usize * 4;
    for row in (0..height as usize).rev() {
        output.extend_from_slice(&top_down[row * stride..(row + 1) * stride]);
    }
    fs::write(path, output)
}
