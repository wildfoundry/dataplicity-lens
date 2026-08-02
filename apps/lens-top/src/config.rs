use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use lens_core::{GroupMode, SortKey};
use serde::{Deserialize, Serialize};

use crate::cli::Args;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigFile {
    pub refresh_interval: String,
    pub default_sort: SortKey,
    pub default_group: GroupMode,
    pub visible_columns: Vec<String>,
    pub theme: String,
    pub colour_mode: String,
    pub history_length: usize,
    pub limit: Option<usize>,
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            refresh_interval: "1s".to_owned(),
            default_sort: SortKey::Cpu,
            default_group: GroupMode::None,
            visible_columns: vec![
                "pid".to_owned(),
                "process".to_owned(),
                "user".to_owned(),
                "cpu".to_owned(),
                "memory".to_owned(),
                "rss".to_owned(),
                "state".to_owned(),
                "threads".to_owned(),
                "io".to_owned(),
                "runtime".to_owned(),
                "service".to_owned(),
            ],
            theme: "default".to_owned(),
            colour_mode: "auto".to_owned(),
            history_length: 60,
            limit: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EffectiveConfig {
    pub interval: Duration,
    pub sort: SortKey,
    pub group: GroupMode,
    pub history_length: usize,
    pub limit: Option<usize>,
    pub no_color: bool,
    pub ascii: bool,
}

impl EffectiveConfig {
    pub fn resolve(args: &Args) -> Result<Self> {
        let path = args.config.clone().unwrap_or_else(default_config_path);
        let mut file = load_optional(&path)?;
        apply_environment(&mut file)?;

        let interval_text = args.interval.as_deref().unwrap_or(&file.refresh_interval);
        let interval = parse_duration(interval_text)?;
        if interval < Duration::from_millis(100) {
            bail!("refresh interval must be at least 100ms");
        }
        Ok(Self {
            interval,
            sort: args.sort.map(Into::into).unwrap_or(file.default_sort),
            group: args.group.map(Into::into).unwrap_or(file.default_group),
            history_length: env_usize("LENS_TOP_HISTORY_LENGTH")?
                .unwrap_or(file.history_length)
                .clamp(2, 3_600),
            limit: args.limit.or(file.limit),
            no_color: args.no_color
                || env_bool("LENS_TOP_NO_COLOR")?.unwrap_or(false)
                || file.colour_mode.eq_ignore_ascii_case("never"),
            ascii: args.ascii || env_bool("LENS_TOP_ASCII")?.unwrap_or(false),
        })
    }
}

pub fn default_config_text() -> Result<String> {
    toml::to_string_pretty(&ConfigFile::default()).context("serialize default configuration")
}

pub fn default_config_path() -> PathBuf {
    if let Some(root) = env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(root).join("dataplicity-lens/config.toml")
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".config/dataplicity-lens/config.toml")
    } else {
        PathBuf::from("dataplicity-lens.toml")
    }
}

fn load_optional(path: &Path) -> Result<ConfigFile> {
    match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents)
            .with_context(|| format!("parse configuration {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ConfigFile::default()),
        Err(error) => Err(error).with_context(|| format!("read configuration {}", path.display())),
    }
}

fn apply_environment(config: &mut ConfigFile) -> Result<()> {
    if let Ok(value) = env::var("LENS_TOP_INTERVAL") {
        config.refresh_interval = value;
    }
    if let Ok(value) = env::var("LENS_TOP_SORT") {
        config.default_sort = parse_sort(&value)?;
    }
    if let Ok(value) = env::var("LENS_TOP_GROUP") {
        config.default_group = parse_group(&value)?;
    }
    if let Some(value) = env_usize("LENS_TOP_LIMIT")? {
        config.limit = Some(value);
    }
    Ok(())
}

fn parse_sort(value: &str) -> Result<SortKey> {
    match value.to_ascii_lowercase().replace('-', "_").as_str() {
        "cpu" => Ok(SortKey::Cpu),
        "memory" | "mem" => Ok(SortKey::Memory),
        "pid" => Ok(SortKey::Pid),
        "name" => Ok(SortKey::Name),
        "user" => Ok(SortKey::User),
        "runtime" => Ok(SortKey::Runtime),
        "read_rate" => Ok(SortKey::ReadRate),
        "write_rate" => Ok(SortKey::WriteRate),
        "threads" => Ok(SortKey::Threads),
        _ => bail!("invalid LENS_TOP_SORT value: {value}"),
    }
}

fn parse_group(value: &str) -> Result<GroupMode> {
    match value.to_ascii_lowercase().as_str() {
        "none" => Ok(GroupMode::None),
        "tree" => Ok(GroupMode::Tree),
        "user" => Ok(GroupMode::User),
        "service" | "cgroup" => Ok(GroupMode::Service),
        _ => bail!("invalid LENS_TOP_GROUP value: {value}"),
    }
}

pub fn parse_duration(value: &str) -> Result<Duration> {
    let value = value.trim();
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1_u64)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else {
        (value, 1_000)
    };
    let amount: f64 = number
        .parse()
        .with_context(|| format!("invalid duration: {value}"))?;
    if !amount.is_finite() || amount <= 0.0 {
        bail!("duration must be a positive finite value");
    }
    Ok(Duration::from_millis((amount * multiplier as f64) as u64))
}

fn env_usize(name: &str) -> Result<Option<usize>> {
    env::var(name)
        .ok()
        .map(|value| {
            value
                .parse()
                .with_context(|| format!("invalid {name} value: {value}"))
        })
        .transpose()
}

fn env_bool(name: &str) -> Result<Option<bool>> {
    env::var(name)
        .ok()
        .map(|value| match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => bail!("invalid {name} value: {value}"),
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duration_units() {
        assert_eq!(
            parse_duration("500ms").expect("duration"),
            Duration::from_millis(500)
        );
        assert_eq!(
            parse_duration("2s").expect("duration"),
            Duration::from_secs(2)
        );
        assert_eq!(
            parse_duration("1m").expect("duration"),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn default_config_round_trips() {
        let text = default_config_text().expect("default config");
        let parsed: ConfigFile = toml::from_str(&text).expect("parse default config");
        assert_eq!(parsed.refresh_interval, "1s");
    }
}
