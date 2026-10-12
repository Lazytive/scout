//! Settings: an optional TOML file (`--config` or `SCOUT_SHARE_CONFIG`) with every key
//! overridable as `SCOUT_SHARE_<KEY>` (what Docker uses). Checked once at start.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// Most upload size a server may set (MB).
const MAX_UPLOAD_MB_LIMIT: u64 = 100_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Address and port to listen on.
    pub listen: String,
    /// How people reach the server, e.g. `https://clips.example.com` (links and the
    /// Open Graph tags Discord reads must be absolute). Needed to serve.
    pub public_url: String,
    /// Clips, tokens and temporary uploads live here.
    pub data_dir: PathBuf,
    /// Largest upload, MB (decimal).
    pub max_upload_mb: u64,
    /// Most space all clips may take, GB (decimal).
    pub quota_gb: u64,
    /// Longest expiry an upload may ask for, days; 0 = no limit ("never" allowed).
    pub max_expiry_days: u32,
    /// Expiry when an upload doesn't ask, days; 0 = never.
    pub default_expiry_days: u32,
    /// Uploads running at once; more get 429.
    pub max_concurrent_uploads: usize,
    /// Shown on the player page and in embeds.
    pub site_name: String,
    /// An upload that sends nothing for this long is dropped (seconds).
    pub upload_idle_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "0.0.0.0:8080".into(),
            public_url: String::new(),
            data_dir: PathBuf::from("data"),
            max_upload_mb: 500,
            quota_gb: 20,
            max_expiry_days: 0,
            default_expiry_days: 0,
            max_concurrent_uploads: 4,
            site_name: "Scout".into(),
            upload_idle_secs: 60,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("can't read {0}: {1}")]
    Read(PathBuf, std::io::Error),
    #[error("{0}: {1}")]
    Parse(PathBuf, String),
    #[error("{0}")]
    Invalid(String),
}

fn invalid(msg: impl Into<String>) -> ConfigError {
    ConfigError::Invalid(msg.into())
}

impl Config {
    /// The file (if any), then the environment, then the checks.
    pub fn load(path: Option<&Path>) -> Result<Config, ConfigError> {
        let mut c = match path {
            Some(p) => {
                let text =
                    std::fs::read_to_string(p).map_err(|e| ConfigError::Read(p.into(), e))?;
                toml::from_str(&text).map_err(|e| ConfigError::Parse(p.into(), e.to_string()))?
            }
            None => Config::default(),
        };
        c.apply_env(|k| std::env::var(k).ok())?;
        c.check()?;
        Ok(c)
    }

    /// `SCOUT_SHARE_<KEY>` overrides.
    pub fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) -> Result<(), ConfigError> {
        fn num<T: std::str::FromStr>(key: &str, v: &str) -> Result<T, ConfigError> {
            v.trim()
                .parse()
                .map_err(|_| invalid(format!("{key} must be a whole number, not {v:?}")))
        }
        let var = |k: &str| get(&format!("SCOUT_SHARE_{k}"));
        if let Some(v) = var("LISTEN") {
            self.listen = v;
        }
        if let Some(v) = var("PUBLIC_URL") {
            self.public_url = v;
        }
        if let Some(v) = var("DATA_DIR") {
            self.data_dir = PathBuf::from(v);
        }
        if let Some(v) = var("MAX_UPLOAD_MB") {
            self.max_upload_mb = num("SCOUT_SHARE_MAX_UPLOAD_MB", &v)?;
        }
        if let Some(v) = var("QUOTA_GB") {
            self.quota_gb = num("SCOUT_SHARE_QUOTA_GB", &v)?;
        }
        if let Some(v) = var("MAX_EXPIRY_DAYS") {
            self.max_expiry_days = num("SCOUT_SHARE_MAX_EXPIRY_DAYS", &v)?;
        }
        if let Some(v) = var("DEFAULT_EXPIRY_DAYS") {
            self.default_expiry_days = num("SCOUT_SHARE_DEFAULT_EXPIRY_DAYS", &v)?;
        }
        if let Some(v) = var("MAX_CONCURRENT_UPLOADS") {
            self.max_concurrent_uploads = num("SCOUT_SHARE_MAX_CONCURRENT_UPLOADS", &v)?;
        }
        if let Some(v) = var("SITE_NAME") {
            self.site_name = v;
        }
        if let Some(v) = var("UPLOAD_IDLE_SECS") {
            self.upload_idle_secs = num("SCOUT_SHARE_UPLOAD_IDLE_SECS", &v)?;
        }
        Ok(())
    }

    /// Everything but `public_url` (the token commands don't need it).
    pub fn check(&mut self) -> Result<(), ConfigError> {
        self.listen_addr()?;
        if !(1..=MAX_UPLOAD_MB_LIMIT).contains(&self.max_upload_mb) {
            return Err(invalid(format!(
                "max_upload_mb must be 1 to {MAX_UPLOAD_MB_LIMIT}"
            )));
        }
        if self.quota_gb == 0 {
            return Err(invalid("quota_gb must be at least 1"));
        }
        if self.max_expiry_days > 0
            && (self.default_expiry_days == 0 || self.default_expiry_days > self.max_expiry_days)
        {
            return Err(invalid(
                "default_expiry_days must be 1 to max_expiry_days when max_expiry_days is set",
            ));
        }
        if !(1..=64).contains(&self.max_concurrent_uploads) {
            return Err(invalid("max_concurrent_uploads must be 1 to 64"));
        }
        if !(1..=3600).contains(&self.upload_idle_secs) {
            return Err(invalid("upload_idle_secs must be 1 to 3600"));
        }
        self.site_name = klip_share::clean_title(&self.site_name);
        if self.site_name.is_empty() {
            self.site_name = "Scout".into();
        }
        if self.data_dir.as_os_str().is_empty() {
            return Err(invalid("data_dir is empty"));
        }
        Ok(())
    }

    /// `public_url` checked and without a trailing `/` (serving needs it).
    pub fn check_public_url(&mut self) -> Result<(), ConfigError> {
        let u = self.public_url.trim().trim_end_matches('/').to_string();
        let ok_scheme = u.starts_with("https://") || u.starts_with("http://");
        let host = u.split_once("://").map(|(_, h)| h).unwrap_or("");
        let bad_char = u.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>' | '\\')
        });
        if !ok_scheme || host.is_empty() || bad_char || u.len() > 200 {
            return Err(invalid(
                "public_url (SCOUT_SHARE_PUBLIC_URL) must be how people reach the server, e.g. https://clips.example.com",
            ));
        }
        self.public_url = u;
        Ok(())
    }

    pub fn listen_addr(&self) -> Result<SocketAddr, ConfigError> {
        self.listen.trim().parse().map_err(|_| {
            invalid(format!(
                "listen must be an address:port, not {:?}",
                self.listen
            ))
        })
    }

    pub fn max_upload_bytes(&self) -> u64 {
        self.max_upload_mb.saturating_mul(1_000_000)
    }

    pub fn quota_bytes(&self) -> u64 {
        self.quota_gb.saturating_mul(1_000_000_000)
    }

    pub fn upload_idle(&self) -> Duration {
        Duration::from_secs(self.upload_idle_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let v: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (format!("SCOUT_SHARE_{k}"), (*v).to_string()))
            .collect();
        move |k| v.iter().find(|(n, _)| n == k).map(|(_, x)| x.clone())
    }

    #[test]
    fn file_then_environment_then_checks() {
        let mut c: Config = toml::from_str("max_upload_mb = 50\nsite_name = 'Mine'").unwrap();
        c.apply_env(env(&[
            ("QUOTA_GB", "3"),
            ("PUBLIC_URL", "https://clips.example.com/"),
        ]))
        .unwrap();
        c.check().unwrap();
        c.check_public_url().unwrap();
        assert_eq!(c.max_upload_bytes(), 50_000_000);
        assert_eq!(c.quota_bytes(), 3_000_000_000);
        assert_eq!(c.public_url, "https://clips.example.com");
        assert_eq!(c.site_name, "Mine");
    }

    #[test]
    fn bad_settings_are_refused() {
        assert!(toml::from_str::<Config>("nope = 1").is_err());
        let mut c = Config::default();
        assert!(c.apply_env(env(&[("QUOTA_GB", "lots")])).is_err());
        for bad in [
            "",
            "clips.example.com",
            "ftp://x",
            "https://",
            "https://a b",
            "https://x\"><script>",
        ] {
            let mut c = Config {
                public_url: bad.into(),
                ..Config::default()
            };
            assert!(c.check_public_url().is_err(), "{bad}");
        }
        for broken in [
            Config {
                max_upload_mb: 0,
                ..Config::default()
            },
            Config {
                quota_gb: 0,
                ..Config::default()
            },
            Config {
                listen: "everywhere".into(),
                ..Config::default()
            },
            Config {
                max_expiry_days: 7,
                default_expiry_days: 0,
                ..Config::default()
            },
            Config {
                max_expiry_days: 7,
                default_expiry_days: 8,
                ..Config::default()
            },
            Config {
                max_concurrent_uploads: 0,
                ..Config::default()
            },
            Config {
                upload_idle_secs: 0,
                ..Config::default()
            },
        ] {
            let mut b = broken;
            assert!(b.check().is_err(), "{b:?}");
        }
    }
}
