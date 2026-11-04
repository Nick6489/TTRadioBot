use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Desired {
    pub url: Option<String>,
    pub volume: u8,
    pub enabled: bool,
}
impl Desired {
    pub fn load(path: &Path, volume: u8) -> Result<Self> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    url: None,
                    volume,
                    enabled: false,
                });
            }
            Err(e) => return Err(e).context("read state"),
        };
        let s: Self = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid state file; fix or remove it explicitly"))?;
        ensure!(s.volume <= 100, "invalid state volume");
        if let Some(u) = &s.url {
            crate::command::validate_url(u)?;
        }
        ensure!(!s.enabled || s.url.is_some(), "enabled state has no URL");
        Ok(s)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let temp = path.with_extension(format!("tmp.{}", std::process::id()));
        let result = (|| {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            f.write_all(&serde_json::to_vec(self)?)?;
            f.sync_all()?;
            fs::rename(&temp, path)?;
            fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Actual {
    Stopped,
    Waiting,
    Connecting,
    Playing,
    Retrying,
}
impl std::fmt::Display for Actual {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
#[derive(Default)]
pub struct Occupancy {
    empty_since: Option<Instant>,
}
impl Occupancy {
    pub fn wants_audio(
        &mut self,
        desired: &Desired,
        ready: bool,
        listeners: bool,
        active: bool,
        now: Instant,
        grace: Duration,
    ) -> bool {
        if !desired.enabled || desired.url.is_none() || !ready {
            self.empty_since = None;
            return false;
        }
        if listeners {
            self.empty_since = None;
            return true;
        }
        let since = *self.empty_since.get_or_insert(now);
        active && now.duration_since(since) < grace
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transitions() {
        let mut o = Occupancy::default();
        let mut d = Desired {
            url: Some("https://radio.test/live".into()),
            volume: 100,
            enabled: true,
        };
        let n = Instant::now();
        let g = Duration::from_secs(5);
        assert!(!o.wants_audio(&d, true, false, false, n, g));
        assert!(o.wants_audio(&d, true, true, false, n, g));
        assert!(o.wants_audio(&d, true, false, true, n, g));
        assert!(o.wants_audio(&d, true, true, true, n + g, g));
        assert!(o.wants_audio(&d, true, false, true, n + g, g));
        assert!(!o.wants_audio(&d, true, false, true, n + g + g, g));
        assert!(o.wants_audio(&d, true, true, false, n + g + g, g));
        d.enabled = false;
        assert!(!o.wants_audio(&d, true, true, false, n, g));
    }
    #[test]
    fn persistence() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state");
        let mut d = Desired::load(&p, 70).unwrap();
        assert!(!d.enabled);
        d.url = Some("https://user:secret@radio.test/live".into());
        d.enabled = true;
        d.save(&p).unwrap();
        assert_eq!(Desired::load(&p, 100).unwrap(), d);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::write(&p, b"bad").unwrap();
        assert!(Desired::load(&p, 100).is_err());
    }
}
