use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderStatus {
    Ok,
    Tight,
    Out,
    Unknown,
}

impl ProviderStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Tight => "tight",
            Self::Out => "out",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "ok" => Self::Ok,
            "tight" => Self::Tight,
            "out" => Self::Out,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SparkStatus {
    Available,
    Busy,
    Down,
    None,
}

impl SparkStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Busy => "busy",
            Self::Down => "down",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub claude: ProviderStatus,
    pub codex: ProviderStatus,
    pub spark: SparkStatus,
    pub recommendation: String,
    pub reason: String,
}

impl Verdict {
    pub fn json(&self) -> Value {
        json!({
            "claude": self.claude.as_str(),
            "codex": self.codex.as_str(),
            "spark": self.spark.as_str(),
            "recommendation": self.recommendation,
            "reason": self.reason
        })
    }
}

pub fn recommend_worker(
    task_size: &str,
    claude: ProviderStatus,
    bulk_recommendation: &str,
) -> &'static str {
    if task_size == "small" && claude != ProviderStatus::Out {
        "claude"
    } else {
        match bulk_recommendation {
            "claude" => "claude",
            "codex" => "codex",
            "spark" => "spark",
            _ => "conserve",
        }
    }
}

pub fn verdict(state: &Value, now: i64) -> Verdict {
    let readings = state["readings"].as_array().cloned().unwrap_or_default();
    let claude_readings = readings
        .iter()
        .filter(|reading| reading["provider"].as_str() == Some("claude"))
        .collect::<Vec<_>>();
    let codex_readings = readings
        .iter()
        .filter(|reading| reading["provider"].as_str() == Some("codex"))
        .collect::<Vec<_>>();
    let (claude, claude_reason) = provider_status(&claude_readings, "Claude", 1800, now);
    let (codex, codex_reason) = provider_status(&codex_readings, "Codex", 21600, now);
    let spark = spark_status(state.get("spark"), now);
    let recommendation = if claude == ProviderStatus::Ok {
        "claude"
    } else if codex == ProviderStatus::Ok {
        "codex"
    } else if spark == SparkStatus::Available {
        "spark"
    } else {
        "conserve"
    };
    let spark_reason = match spark {
        SparkStatus::Available => Some("Spark available".to_string()),
        SparkStatus::Busy => Some("Spark busy".to_string()),
        SparkStatus::Down => Some("Spark down".to_string()),
        SparkStatus::None => None,
    };
    let reason = [claude_reason, codex_reason, spark_reason]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("; ");
    Verdict {
        claude,
        codex,
        spark,
        recommendation: recommendation.into(),
        reason,
    }
}

fn provider_status(
    readings: &[&Value],
    title: &str,
    stale_after: i64,
    now: i64,
) -> (ProviderStatus, Option<String>) {
    if readings.is_empty() {
        return (
            ProviderStatus::Unknown,
            Some(format!("{title} unavailable")),
        );
    }
    let mut worst = ProviderStatus::Ok;
    let mut worst_rank = 0;
    let mut reason = None;
    for reading in readings {
        let stale = now - reading["as_of"].as_i64().unwrap_or(0) > stale_after;
        if stale {
            if worst_rank < 1 {
                worst = ProviderStatus::Unknown;
                worst_rank = 1;
                reason = Some(format!("{title} data stale"));
            }
            continue;
        }
        let Some(windows) = reading["windows"].as_array() else {
            if worst_rank < 1 {
                worst = ProviderStatus::Unknown;
                worst_rank = 1;
                reason = Some(format!("{title} has no readings"));
            }
            continue;
        };
        if windows.is_empty() && worst_rank < 1 {
            worst = ProviderStatus::Unknown;
            worst_rank = 1;
            reason = Some(format!("{title} has no readings"));
        }
        for window in windows {
            let percent = window["percent"].as_f64().unwrap_or(0.0);
            let pace = crate::providers::pace(
                percent,
                window["resets_at"].as_i64(),
                window["window_secs"].as_i64(),
                now,
                None,
            );
            let status = if percent >= 98.0 {
                ProviderStatus::Out
            } else if percent >= 85.0 || pace.full_at.is_some() {
                ProviderStatus::Tight
            } else {
                ProviderStatus::Ok
            };
            let rank = match status {
                ProviderStatus::Out => 4,
                ProviderStatus::Tight => 3,
                ProviderStatus::Unknown => 1,
                ProviderStatus::Ok => 0,
            };
            if rank >= worst_rank {
                worst = status;
                worst_rank = rank;
                reason = Some(window_reason(title, window, percent, pace, now));
            }
        }
    }
    (worst, reason)
}

fn window_reason(
    title: &str,
    window: &Value,
    percent: f64,
    pace: crate::providers::Pace,
    now: i64,
) -> String {
    let label = window["label"].as_str().unwrap_or("window");
    match (pace.full_at, window["resets_at"].as_i64()) {
        (Some(full), Some(reset)) => format!(
            "{title} {label} {:.0}% used, projected empty {} before its {} reset",
            percent,
            crate::local_time(full, full - now > 86400),
            crate::local_time(reset, reset - now > 86400)
        ),
        _ => format!("{title} {label} {:.0}%, on pace", percent),
    }
}

fn spark_status(spark: Option<&Value>, now: i64) -> SparkStatus {
    let Some(spark) = spark.filter(|value| !value.is_null()) else {
        return SparkStatus::None;
    };
    if now - spark["fetched_at"].as_i64().unwrap_or(0) > 600
        || !spark["up"].as_bool().unwrap_or(false)
        || spark["error"].is_string()
    {
        return SparkStatus::Down;
    }
    let running = spark["endpoints"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|endpoint| endpoint["running"].as_i64())
        .sum::<i64>();
    if running <= 2 {
        SparkStatus::Available
    } else {
        SparkStatus::Busy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(percent: f64, reset: i64, seconds: i64) -> Value {
        json!({"key":"5h","label":"5-hour","percent":percent,"resets_at":reset,"window_secs":seconds})
    }

    #[test]
    fn classifies_limits_staleness_and_routing() {
        let now = 10_000;
        let state = json!({
            "readings": [
                {"provider":"claude","as_of":now,"windows":[window(88.0, now + 1000, 18000)]},
                {"provider":"codex","as_of":now,"windows":[window(47.0, now + 300000, 604800)]}
            ],
            "spark":{"fetched_at":now,"up":true,"endpoints":[{"running":0}]}
        });
        let result = verdict(&state, now);
        assert_eq!(result.claude, ProviderStatus::Tight);
        assert_eq!(result.codex, ProviderStatus::Ok);
        assert_eq!(result.spark, SparkStatus::Available);
        assert_eq!(result.recommendation, "codex");
        assert_eq!(
            recommend_worker("small", result.claude, &result.recommendation),
            "claude"
        );
        assert_eq!(
            recommend_worker("large", result.claude, &result.recommendation),
            "codex"
        );

        let stale = json!({"readings":[{"provider":"claude","as_of":now-1801,"windows":[window(1.0,now+10,18000)]}]});
        assert_eq!(verdict(&stale, now).claude, ProviderStatus::Unknown);
    }

    #[test]
    fn out_and_spark_thresholds() {
        let now = 20_000;
        let state = json!({
            "readings":[{"provider":"claude","as_of":now,"windows":[window(98.0,now+1000,18000)]}],
            "spark":{"fetched_at":now,"up":true,"endpoints":[{"running":3}]}
        });
        let result = verdict(&state, now);
        assert_eq!(result.claude, ProviderStatus::Out);
        assert_eq!(result.spark, SparkStatus::Busy);
        assert_eq!(result.recommendation, "conserve");
        assert_eq!(
            recommend_worker("small", result.claude, &result.recommendation),
            "conserve"
        );
    }

    #[test]
    fn fetch_errors_do_not_change_quota_status() {
        let now = 30_000;
        let empty = json!({
            "readings":[{"provider":"claude","as_of":now,"error":"HTTP 429 rate limited","windows":[]}]
        });
        let empty_result = verdict(&empty, now);
        assert_eq!(empty_result.claude, ProviderStatus::Unknown);
        assert!(!empty_result.reason.contains("rate limit"));

        let cached = json!({
            "readings":[{"provider":"claude","as_of":now,"error":"fetch failed","windows":[window(42.0,now+9000,18000)]}]
        });
        assert_eq!(verdict(&cached, now).claude, ProviderStatus::Ok);
    }
}
