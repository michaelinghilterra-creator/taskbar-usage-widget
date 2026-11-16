use crate::router::ProviderStatus;
use serde_json::{json, Value};
use std::fs;
use std::io::{self, Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookEvent {
    UserPromptSubmit,
    SessionStart,
}

impl HookEvent {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "UserPromptSubmit" => Some(Self::UserPromptSubmit),
            "SessionStart" => Some(Self::SessionStart),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::SessionStart => "SessionStart",
        }
    }
}

pub fn run() {
    let _ = run_inner();
}

fn run_inner() -> Result<(), ()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).map_err(|_| ())?;
    let request: Value = serde_json::from_str(&input).map_err(|_| ())?;
    let event =
        HookEvent::parse(request["hook_event_name"].as_str().unwrap_or_default()).ok_or(())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let state = crate::mcp::load_cached_state().ok_or(())?;
    let verdict = crate::router::verdict(&state, now);
    let previous = load_hook_state();
    if !should_speak(event, verdict.claude, previous.as_ref(), now) {
        return Ok(());
    }
    let recovered = verdict.claude == ProviderStatus::Ok;
    let text = if recovered {
        "Claude headroom is back.".to_string()
    } else {
        format!(
            "Usage headroom (taskbar widget, {}): {}. Suggested worker for bulk coding: {}.",
            crate::local_time(now, false),
            verdict.reason.trim_end_matches('.'),
            title(&verdict.recommendation)
        )
    };
    let output = json!({"hookSpecificOutput": {
        "hookEventName": event.as_str(),
        "additionalContext": text.chars().take(599).collect::<String>()
    }});
    let mut stdout = io::stdout().lock();
    let _ = serde_json::to_writer(&mut stdout, &output);
    let _ = stdout.write_all(b"\n");
    let saved = json!({"state": verdict.claude.as_str(), "spoken_at": now});
    let _ = save_hook_state(&saved);
    Ok(())
}

fn title(value: &str) -> &str {
    match value {
        "claude" => "Claude",
        "codex" => "Codex",
        "spark" => "Spark",
        _ => "Conserve",
    }
}

fn load_hook_state() -> Option<Value> {
    fs::read(crate::storage::dir().join("hook-state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn save_hook_state(value: &Value) -> Result<(), String> {
    fs::create_dir_all(crate::storage::dir()).map_err(|error| error.to_string())?;
    crate::storage::write_atomic(
        &crate::storage::dir().join("hook-state.json"),
        &serde_json::to_vec(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn should_speak(
    event: HookEvent,
    current: ProviderStatus,
    previous: Option<&Value>,
    now: i64,
) -> bool {
    if current == ProviderStatus::Unknown {
        return false;
    }
    let prior = previous.and_then(|value| value["state"].as_str());
    let spoken_at = previous
        .and_then(|value| value["spoken_at"].as_i64())
        .unwrap_or(0);
    if current == ProviderStatus::Ok {
        return matches!(prior, Some("tight" | "out"));
    }
    if event == HookEvent::SessionStart {
        return true;
    }
    prior != Some(current.as_str()) || now - spoken_at >= 1800
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prior(state: &str, time: i64) -> Value {
        json!({"state":state,"spoken_at":time})
    }

    #[test]
    fn speak_decision_table() {
        let now = 10_000;
        assert!(should_speak(
            HookEvent::UserPromptSubmit,
            ProviderStatus::Tight,
            None,
            now
        ));
        let tight = prior("tight", now - 60);
        assert!(!should_speak(
            HookEvent::UserPromptSubmit,
            ProviderStatus::Tight,
            Some(&tight),
            now
        ));
        let old_tight = prior("tight", now - 1800);
        assert!(should_speak(
            HookEvent::UserPromptSubmit,
            ProviderStatus::Tight,
            Some(&old_tight),
            now
        ));
        assert!(should_speak(
            HookEvent::UserPromptSubmit,
            ProviderStatus::Ok,
            Some(&tight),
            now
        ));
        assert!(!should_speak(
            HookEvent::UserPromptSubmit,
            ProviderStatus::Unknown,
            Some(&tight),
            now
        ));
        assert!(should_speak(
            HookEvent::SessionStart,
            ProviderStatus::Out,
            Some(&tight),
            now
        ));
    }

    #[test]
    fn fetch_error_without_windows_is_unknown_and_silent() {
        let now = 20_000;
        let state = json!({
            "readings":[{"provider":"claude","as_of":now,"error":"HTTP 429","windows":[]}]
        });
        let status = crate::router::verdict(&state, now).claude;
        assert_eq!(status, ProviderStatus::Unknown);
        assert!(!should_speak(
            HookEvent::UserPromptSubmit,
            status,
            None,
            now
        ));
    }

    #[test]
    fn fetch_error_with_fresh_windows_uses_window_status() {
        let now = 20_000;
        let state = json!({
            "readings":[{"provider":"claude","as_of":now,"error":"fetch failed","windows":[{
                "key":"5h","label":"5-hour","percent":98.0,"resets_at":now+1000,"window_secs":18000
            }]}]
        });
        let status = crate::router::verdict(&state, now).claude;
        assert_eq!(status, ProviderStatus::Out);
        assert!(should_speak(HookEvent::UserPromptSubmit, status, None, now));
    }
}
