//! Local request statistics collected by the private WebView bridge.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::NaiveDate;
use serde::Deserialize;

use super::ChatWidget;

/// Time range for local Web2Codex request statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsagePeriod {
    Daily,
    Weekly,
    Cumulative,
}

impl UsagePeriod {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "day" | "daily" => Some(Self::Daily),
            "week" | "weekly" => Some(Self::Weekly),
            "cumulative" => Some(Self::Cumulative),
            _ => None,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct LocalUsageStats {
    version: u8,
    total: UsageBucket,
    days: BTreeMap<String, UsageBucket>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct UsageBucket {
    requests: u64,
    completed_responses: u64,
    failed_responses: u64,
    input_characters: u64,
    output_characters: u64,
    response_milliseconds: u64,
    model_requests: BTreeMap<String, u64>,
}

impl UsageBucket {
    fn add_assign(&mut self, other: &Self) {
        self.requests = self.requests.saturating_add(other.requests);
        self.completed_responses = self
            .completed_responses
            .saturating_add(other.completed_responses);
        self.failed_responses = self.failed_responses.saturating_add(other.failed_responses);
        self.input_characters = self.input_characters.saturating_add(other.input_characters);
        self.output_characters = self
            .output_characters
            .saturating_add(other.output_characters);
        self.response_milliseconds = self
            .response_milliseconds
            .saturating_add(other.response_milliseconds);
        for (model, count) in &other.model_requests {
            let total = self.model_requests.entry(model.clone()).or_default();
            *total = total.saturating_add(*count);
        }
    }
}

impl ChatWidget {
    pub(crate) fn show_local_usage(&mut self, view: UsagePeriod) {
        let Some(path) = local_usage_stats_path() else {
            self.add_info_message(
                "Local Web2Codex usage statistics are not available on this system yet.".into(),
                None,
            );
            return;
        };
        let Some(stats) = std::fs::read(path)
            .ok()
            .and_then(|contents| serde_json::from_slice::<LocalUsageStats>(&contents).ok())
            .filter(|stats| stats.version == 1)
        else {
            self.add_info_message(
                "No local Web2Codex responses have been recorded yet. Complete a response in the private WebView, then run `/usage` again.".into(),
                None,
            );
            return;
        };

        let today = chrono::Utc::now().date_naive();
        let mut usage = UsageBucket::default();
        let period = match view {
            UsagePeriod::Daily => {
                if let Some(bucket) = stats.days.get(&today.to_string()) {
                    usage.add_assign(bucket);
                }
                "Today (UTC)"
            }
            UsagePeriod::Weekly => {
                let start = today - chrono::Duration::days(6);
                for (day, bucket) in &stats.days {
                    if NaiveDate::parse_from_str(day, "%Y-%m-%d")
                        .is_ok_and(|date| date >= start && date <= today)
                    {
                        usage.add_assign(bucket);
                    }
                }
                "Last 7 days (UTC)"
            }
            UsagePeriod::Cumulative => {
                usage.add_assign(&stats.total);
                "All time"
            }
        };
        self.add_info_message(render_usage(&usage, period), None);
    }
}

fn render_usage(usage: &UsageBucket, period: &str) -> String {
    let average = if usage.requests == 0 {
        "—".to_string()
    } else {
        let milliseconds = usage.response_milliseconds / usage.requests;
        format!(
            "{}.{:01}s",
            milliseconds / 1000,
            (milliseconds % 1000) / 100
        )
    };
    let mut models = usage.model_requests.iter().collect::<Vec<_>>();
    models.sort_by(|(left_name, left_count), (right_name, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_name.cmp(right_name))
    });
    let models = models
        .into_iter()
        .take(5)
        .map(|(name, count)| format!("{name}: {count}"))
        .collect::<Vec<_>>();
    let models = if models.is_empty() {
        "—".to_string()
    } else {
        models.join(", ")
    };

    format!(
        "Local Web2Codex usage — {period}\nRequests: {} ({} completed, {} failed)\nInput: {} characters\nOutput: {} characters\nAverage request time: {average}\nRequests by model: {models}\nChatGPT Web does not provide exact token counts to the local harness, so this report counts requests and characters only.",
        usage.requests,
        usage.completed_responses,
        usage.failed_responses,
        usage.input_characters,
        usage.output_characters,
    )
}

fn local_usage_stats_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("WEB2CODEX_WEB_DATA_DIR") {
        return Some(PathBuf::from(path).join("usage-stats.json"));
    }

    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)?;
    let data_dir = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"))
            .join("Web2Codex/Browser")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Web2Codex/Browser")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("web2codex/browser")
    };
    Some(data_dir.join("usage-stats.json"))
}
