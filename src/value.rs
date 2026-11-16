use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const EMBEDDED_PRICING: &str = include_str!("pricing.json");

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub output: u64,
}

impl Usage {
    pub fn total(self) -> u64 {
        self.input
            .saturating_add(self.cache_write_5m)
            .saturating_add(self.cache_write_1h)
            .saturating_add(self.cache_read)
            .saturating_add(self.output)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectShare {
    pub name: String,
    pub share: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderValue {
    pub today_usd: f64,
    pub week_usd: f64,
    pub month_usd: f64,
    pub last_30_usd: f64,
    pub today_tokens: u64,
    pub top_projects_today: Vec<ProjectShare>,
    pub unpriced: Vec<String>,
    pub calculating: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub claude: ProviderValue,
    pub codex: ProviderValue,
    pub pricing_as_of: String,
}

#[derive(Clone, Debug, Default)]
struct Aggregate {
    usage: Usage,
    usd: f64,
    priced: bool,
}

pub struct Pricing {
    root: Value,
}

impl Pricing {
    #[cfg(test)]
    pub fn embedded_for_test() -> Self {
        Self {
            root: serde_json::from_str(EMBEDDED_PRICING).expect("embedded pricing"),
        }
    }

    pub fn load() -> Self {
        let mut root: Value = serde_json::from_str(EMBEDDED_PRICING).expect("embedded pricing");
        let path = crate::storage::dir().join("pricing.json");
        if let Ok(bytes) = fs::read(path) {
            if let Ok(override_root) = serde_json::from_slice::<Value>(&bytes) {
                for provider in ["claude", "claude_fast", "openai"] {
                    if let Some(models) = override_root[provider].as_object() {
                        if let Some(target) = root[provider].as_object_mut() {
                            for (model, rates) in models {
                                target.insert(model.clone(), rates.clone());
                            }
                        }
                    }
                }
            }
        }
        Self { root }
    }

    pub fn as_of(&self) -> &str {
        self.root["as_of"].as_str().unwrap_or("2026-11-16")
    }

    fn rate(value: &Value, key: &str) -> f64 {
        value[key].as_f64().unwrap_or(0.0)
    }

    pub fn claude_cost(&self, model: &str, usage: Usage, fast: bool, us_geo: bool) -> Option<f64> {
        let model = normalize_model(model)?;
        let mut rates = self.root["claude"].get(&model)?;
        let prompt = usage.input + usage.cache_write_5m + usage.cache_write_1h + usage.cache_read;
        if prompt
            > rates["long"]["over_prompt_tokens"]
                .as_u64()
                .unwrap_or(u64::MAX)
        {
            rates = &rates["long"];
        }
        let mut input = Self::rate(rates, "input");
        let mut output = Self::rate(rates, "output");
        let mut cw5 = Self::rate(rates, "cache_write_5m");
        let mut cw1 = Self::rate(rates, "cache_write_1h");
        let mut read = Self::rate(rates, "cache_read");
        if fast {
            if let Some(fast_rates) = self.root["claude_fast"].get(&model) {
                let fast_input = Self::rate(fast_rates, "input");
                let scale = if input == 0.0 {
                    1.0
                } else {
                    fast_input / input
                };
                input = fast_input;
                output = Self::rate(fast_rates, "output");
                cw5 *= scale;
                cw1 *= scale;
                read *= scale;
            }
        }
        let mut cost = (usage.input as f64 * input
            + usage.cache_write_5m as f64 * cw5
            + usage.cache_write_1h as f64 * cw1
            + usage.cache_read as f64 * read
            + usage.output as f64 * output)
            / 1_000_000.0;
        if us_geo {
            cost *= self.root["claude_us_geo_multiplier"]
                .as_f64()
                .unwrap_or(1.1);
        }
        Some(cost)
    }

    pub fn codex_cost(&self, model: &str, input: u64, cached: u64, output: u64) -> Option<f64> {
        let model = normalize_model(model)?;
        let rates = self.root["openai"].get(&model)?;
        Some(
            (input.saturating_sub(cached) as f64 * Self::rate(rates, "input")
                + cached as f64 * Self::rate(rates, "cached_input")
                + output as f64 * Self::rate(rates, "output"))
                / 1_000_000.0,
        )
    }
}

pub fn normalize_model(model: &str) -> Option<String> {
    if model == "<synthetic>" || model.is_empty() {
        return None;
    }
    let bytes = model.as_bytes();
    if bytes.len() > 9
        && bytes[bytes.len() - 9] == b'-'
        && bytes[bytes.len() - 8..].iter().all(u8::is_ascii_digit)
    {
        Some(model[..model.len() - 9].to_owned())
    } else {
        Some(model.to_owned())
    }
}

pub fn project_key(cwd: Option<&str>) -> String {
    cwd.and_then(|path| {
        path.trim_end_matches(['\\', '/'])
            .rsplit(['\\', '/'])
            .find(|part| !part.is_empty())
    })
    .filter(|part| !part.is_empty())
    .unwrap_or("other")
    .to_owned()
}

#[derive(Default)]
pub struct Store {
    offsets: HashMap<PathBuf, u64>,
    seen: HashMap<String, String>,
    buckets: BTreeMap<String, HashMap<String, HashMap<String, HashMap<String, Aggregate>>>>,
    loaded: bool,
    pub backfill_done: bool,
}

impl Store {
    #[cfg(test)]
    pub fn empty_for_test() -> Self {
        Self {
            loaded: true,
            ..Self::default()
        }
    }

    pub fn load() -> Self {
        let mut store = Self {
            loaded: true,
            ..Self::default()
        };
        let Ok(bytes) = fs::read(crate::storage::dir().join("value.json")) else {
            return store;
        };
        let Ok(root) = serde_json::from_slice::<Value>(&bytes) else {
            return store;
        };
        store.backfill_done = root["backfill_done"].as_bool().unwrap_or(false);
        for (path, offset) in root["offsets"].as_object().into_iter().flatten() {
            store
                .offsets
                .insert(PathBuf::from(path), offset.as_u64().unwrap_or(0));
        }
        for (id, day) in root["seen"].as_object().into_iter().flatten() {
            if let Some(day) = day.as_str() {
                store.seen.insert(id.clone(), day.to_owned());
            }
        }
        for (day, providers) in root["days"].as_object().into_iter().flatten() {
            for (provider, models) in providers.as_object().into_iter().flatten() {
                for (model, projects) in models.as_object().into_iter().flatten() {
                    for (project, value) in projects.as_object().into_iter().flatten() {
                        let usage = Usage {
                            input: value["input"].as_u64().unwrap_or(0),
                            cache_write_5m: value["cache_write_5m"].as_u64().unwrap_or(0),
                            cache_write_1h: value["cache_write_1h"].as_u64().unwrap_or(0),
                            cache_read: value["cache_read"].as_u64().unwrap_or(0),
                            output: value["output"].as_u64().unwrap_or(0),
                        };
                        store.add(
                            day,
                            provider,
                            model,
                            project,
                            usage,
                            value["usd"].as_f64(),
                            value["priced"].as_bool().unwrap_or(false),
                        );
                    }
                }
            }
        }
        store
    }

    pub fn ensure_loaded(&mut self) {
        if !self.loaded {
            *self = Self::load();
        }
    }
    pub fn offset(&self, path: &Path) -> u64 {
        self.offsets.get(path).copied().unwrap_or(0)
    }
    pub fn set_offset(&mut self, path: &Path, offset: u64) {
        self.offsets.insert(path.to_owned(), offset);
    }
    pub fn mark_seen(&mut self, id: &str, day: &str) -> bool {
        if self.seen.contains_key(id) {
            false
        } else {
            self.seen.insert(id.to_owned(), day.to_owned());
            true
        }
    }
    pub fn add(
        &mut self,
        day: &str,
        provider: &str,
        model: &str,
        project: &str,
        usage: Usage,
        usd: Option<f64>,
        priced: bool,
    ) {
        let aggregate = self
            .buckets
            .entry(day.to_owned())
            .or_default()
            .entry(provider.to_owned())
            .or_default()
            .entry(model.to_owned())
            .or_default()
            .entry(project.to_owned())
            .or_default();
        aggregate.usage.input = aggregate.usage.input.saturating_add(usage.input);
        aggregate.usage.cache_write_5m = aggregate
            .usage
            .cache_write_5m
            .saturating_add(usage.cache_write_5m);
        aggregate.usage.cache_write_1h = aggregate
            .usage
            .cache_write_1h
            .saturating_add(usage.cache_write_1h);
        aggregate.usage.cache_read = aggregate.usage.cache_read.saturating_add(usage.cache_read);
        aggregate.usage.output = aggregate.usage.output.saturating_add(usage.output);
        aggregate.usd += usd.unwrap_or(0.0);
        aggregate.priced |= priced;
    }

    pub fn retain(&mut self, today: (i32, u32, u32)) {
        let cutoff = days_from_civil(today.0, today.1, today.2) - 61;
        self.buckets.retain(|day, _| {
            parse_day(day).is_some_and(|d| days_from_civil(d.0, d.1, d.2) >= cutoff)
        });
        let seen_cutoff = days_from_civil(today.0, today.1, today.2) - 1;
        self.seen.retain(|_, day| {
            parse_day(day).is_some_and(|d| days_from_civil(d.0, d.1, d.2) >= seen_cutoff)
        });
    }

    pub fn snapshot(&self, today: (i32, u32, u32), pricing: &Pricing) -> Snapshot {
        Snapshot {
            claude: self.provider_snapshot("claude", today),
            codex: self.provider_snapshot("codex", today),
            pricing_as_of: pricing.as_of().to_owned(),
        }
    }

    pub fn today_usage(&self, provider: &str, today: (i32, u32, u32)) -> Usage {
        let mut total = Usage::default();
        let Some(models) = self
            .buckets
            .get(&day_string(today))
            .and_then(|p| p.get(provider))
        else {
            return total;
        };
        for aggregate in models.values().flat_map(|projects| projects.values()) {
            total.input = total.input.saturating_add(aggregate.usage.input);
            total.cache_write_5m = total
                .cache_write_5m
                .saturating_add(aggregate.usage.cache_write_5m);
            total.cache_write_1h = total
                .cache_write_1h
                .saturating_add(aggregate.usage.cache_write_1h);
            total.cache_read = total.cache_read.saturating_add(aggregate.usage.cache_read);
            total.output = total.output.saturating_add(aggregate.usage.output);
        }
        total
    }

    fn provider_snapshot(&self, provider: &str, today: (i32, u32, u32)) -> ProviderValue {
        let today_n = days_from_civil(today.0, today.1, today.2);
        let weekday = (today_n + 3).rem_euclid(7); // Monday = 0
        let week_start = today_n - weekday;
        let month_start = days_from_civil(today.0, today.1, 1);
        let mut result = ProviderValue {
            calculating: !self.backfill_done,
            ..ProviderValue::default()
        };
        let mut projects: HashMap<String, (f64, u64)> = HashMap::new();
        let mut unpriced = HashSet::new();
        for (day, providers) in &self.buckets {
            let Some(date) = parse_day(day) else { continue };
            let n = days_from_civil(date.0, date.1, date.2);
            let Some(models) = providers.get(provider) else {
                continue;
            };
            for (model, by_project) in models {
                for (project, aggregate) in by_project {
                    if n == today_n {
                        result.today_usd += aggregate.usd;
                        result.today_tokens =
                            result.today_tokens.saturating_add(aggregate.usage.total());
                        let entry = projects.entry(project.clone()).or_default();
                        entry.0 += aggregate.usd;
                        entry.1 = entry.1.saturating_add(aggregate.usage.total());
                        if !aggregate.priced {
                            unpriced.insert(model.clone());
                        }
                    }
                    if n >= week_start && n <= today_n {
                        result.week_usd += aggregate.usd;
                    }
                    if n >= month_start && n <= today_n {
                        result.month_usd += aggregate.usd;
                    }
                    if n > today_n - 30 && n <= today_n {
                        result.last_30_usd += aggregate.usd;
                    }
                }
            }
        }
        let use_usd = projects.values().map(|v| v.0).sum::<f64>() > 0.0;
        let total: f64 = if use_usd {
            projects.values().map(|v| v.0).sum()
        } else {
            projects.values().map(|v| v.1 as f64).sum()
        };
        let mut shares = projects
            .into_iter()
            .map(|(name, (usd, tokens))| ProjectShare {
                name,
                share: if total == 0.0 {
                    0.0
                } else if use_usd {
                    usd / total
                } else {
                    tokens as f64 / total
                },
            })
            .collect::<Vec<_>>();
        shares.sort_by(|a, b| b.share.total_cmp(&a.share));
        if shares.len() > 3 {
            let rest = shares[2..].iter().map(|s| s.share).sum();
            shares.truncate(2);
            shares.push(ProjectShare {
                name: "other".into(),
                share: rest,
            });
        }
        result.top_projects_today = shares;
        result.unpriced = unpriced.into_iter().collect();
        result.unpriced.sort();
        result
    }

    pub fn save(&self) -> Result<(), String> {
        fs::create_dir_all(crate::storage::dir()).map_err(|e| e.to_string())?;
        let days = self.buckets.iter().map(|(day, providers)| {
            let providers = providers.iter().map(|(provider, models)| {
                let models = models.iter().map(|(model, projects)| {
                    let projects = projects.iter().map(|(project, a)| (project.clone(), json!({
                        "input":a.usage.input,"cache_write_5m":a.usage.cache_write_5m,"cache_write_1h":a.usage.cache_write_1h,"cache_read":a.usage.cache_read,"output":a.usage.output,"usd":a.usd,"priced":a.priced
                    }))).collect::<serde_json::Map<_,_>>();
                    (model.clone(), Value::Object(projects))
                }).collect::<serde_json::Map<_,_>>();
                (provider.clone(), Value::Object(models))
            }).collect::<serde_json::Map<_,_>>();
            (day.clone(), Value::Object(providers))
        }).collect::<serde_json::Map<_,_>>();
        let offsets = self
            .offsets
            .iter()
            .map(|(path, offset)| (path.to_string_lossy().into_owned(), json!(offset)))
            .collect::<serde_json::Map<_, _>>();
        let seen = self
            .seen
            .iter()
            .map(|(id, day)| (id.clone(), json!(day)))
            .collect::<serde_json::Map<_, _>>();
        let bytes = serde_json::to_vec(&json!({"version":1,"backfill_done":self.backfill_done,"offsets":offsets,"seen":seen,"days":days})).unwrap();
        crate::storage::write_atomic(&crate::storage::dir().join("value.json"), &bytes)
            .map_err(|e| e.to_string())
    }
}

pub fn day_string(day: (i32, u32, u32)) -> String {
    format!("{:04}-{:02}-{:02}", day.0, day.1, day.2)
}
fn parse_day(value: &str) -> Option<(i32, u32, u32)> {
    let mut p = value.split('-');
    Some((
        p.next()?.parse().ok()?,
        p.next()?.parse().ok()?,
        p.next()?.parse().ok()?,
    ))
}

// Howard Hinnant's civil calendar conversion, with 1970-01-01 = 0.
pub fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = year - i32::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i32;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day as i32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe - 719468) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pricing() -> Pricing {
        Pricing {
            root: serde_json::from_str(EMBEDDED_PRICING).unwrap(),
        }
    }

    #[test]
    fn strips_date_suffix_and_synthetic() {
        assert_eq!(
            normalize_model("claude-haiku-4-5-20251001").as_deref(),
            Some("claude-haiku-4-5")
        );
        assert_eq!(normalize_model("<synthetic>"), None);
    }
    #[test]
    fn project_basename_and_missing() {
        assert_eq!(project_key(Some(r"C:\work\alpha")), "alpha");
        assert_eq!(project_key(None), "other");
    }
    #[test]
    fn claude_split_and_fallback_costs() {
        let p = pricing();
        let split = Usage {
            input: 1_000_000,
            cache_write_5m: 1_000_000,
            cache_write_1h: 1_000_000,
            cache_read: 1_000_000,
            output: 1_000_000,
        };
        assert!(
            (p.claude_cost("claude-sonnet-4-5", split, false, false)
                .unwrap()
                - 28.05)
                .abs()
                < 1e-9
        );
        let fallback = Usage {
            cache_write_1h: 1_000_000,
            ..Default::default()
        };
        assert!(
            (p.claude_cost("claude-sonnet-4-5", fallback, false, false)
                .unwrap()
                - 6.0)
                .abs()
                < 1e-9
        );
    }
    #[test]
    fn haiku_long_fast_and_geo() {
        let p = pricing();
        let long = Usage {
            input: 100_001,
            ..Default::default()
        };
        assert!(
            (p.claude_cost("claude-haiku-5-5", long, false, false)
                .unwrap()
                - 0.0500005)
                .abs()
                < 1e-9
        );
        let fast = Usage {
            input: 1_000_000,
            cache_read: 1_000_000,
            output: 1_000_000,
            ..Default::default()
        };
        assert!((p.claude_cost("claude-opus-5-5", fast, true, false).unwrap() - 48.4).abs() < 1e-9);
        let one = Usage {
            input: 1_000_000,
            output: 1_000_000,
            ..Default::default()
        };
        assert!((p.claude_cost("claude-opus-5-5", one, false, true).unwrap() - 26.4).abs() < 1e-9);
    }
    #[test]
    fn codex_cached_split() {
        assert!(
            (pricing()
                .codex_cost("gpt-5.6-sol", 1_000_000, 250_000, 100_000)
                .unwrap()
                - 5.1)
                .abs()
                < 1e-9
        );
    }
    #[test]
    fn week_and_month_buckets_cross_boundary() {
        let mut s = Store {
            loaded: true,
            backfill_done: true,
            ..Default::default()
        };
        let u = Usage {
            input: 1,
            ..Default::default()
        };
        s.add("2026-10-31", "claude", "m", "p", u, Some(1.0), true);
        s.add("2026-11-01", "claude", "m", "p", u, Some(2.0), true);
        s.add("2026-11-02", "claude", "m", "p", u, Some(4.0), true);
        let v = s.provider_snapshot("claude", (2026, 11, 2));
        assert_eq!(v.week_usd, 4.0);
        assert_eq!(v.month_usd, 6.0);
        assert_eq!(v.last_30_usd, 7.0);
    }
}
