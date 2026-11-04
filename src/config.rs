use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{fs, path::PathBuf};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    pub tcp_port: i32,
    pub udp_port: i32,
    pub tls: bool,
    #[serde(default)]
    pub insecure_tls: bool,
    pub ca_file: Option<String>,
    pub sdk_library: PathBuf,
    pub username: String,
    pub password: String,
    pub nickname: String,
    pub channel: String,
    #[serde(default)]
    pub channel_password: String,
    pub controllers: Vec<String>,
    #[serde(default)]
    pub excluded_listeners: Vec<String>,
    #[serde(default)]
    pub channel_commands: bool,
    #[serde(default = "volume")]
    pub default_volume: u8,
    #[serde(default = "grace")]
    pub grace_seconds: u64,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "backoff")]
    pub max_backoff_seconds: u64,
    pub state_file: PathBuf,
}
fn volume() -> u8 {
    100
}
fn grace() -> u64 {
    5
}
fn timeout() -> u64 {
    10
}
fn backoff() -> u64 {
    60
}
impl Config {
    pub fn load(path: &str) -> Result<Self> {
        let c: Self = toml::from_str(&fs::read_to_string(path).context("read configuration")?)
            .map_err(|_| {
                anyhow::anyhow!("invalid TOML configuration; check field names and types")
            })?;
        ensure!(c.default_volume <= 100, "default_volume must be 0–100");
        ensure!(
            (1..=65535).contains(&c.tcp_port) && (1..=65535).contains(&c.udp_port),
            "invalid ports"
        );
        ensure!(
            (1..=60).contains(&c.timeout_seconds)
                && (1..=300).contains(&c.max_backoff_seconds)
                && c.grace_seconds <= 60,
            "invalid timeout/backoff/grace"
        );
        ensure!(
            c.controllers.iter().all(|s| !s.trim().is_empty()),
            "empty controller account"
        );
        ensure!(
            c.tls || (!c.insecure_tls && c.ca_file.is_none()),
            "TLS settings require tls=true"
        );
        Ok(c)
    }
}
