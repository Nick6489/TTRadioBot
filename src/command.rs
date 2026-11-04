use anyhow::{Result, bail, ensure};
#[derive(Debug, PartialEq)]
pub enum Command {
    Url(String),
    Stop,
    Play,
    Volume(u8),
    Info,
    Home,
    Help,
}
pub fn validate_url(s: &str) -> Result<()> {
    let u = url::Url::parse(s).map_err(|_| anyhow::anyhow!("Invalid URL"))?;
    ensure!(
        matches!(u.scheme(), "http" | "https") && u.host_str().is_some() && s.len() <= 8192,
        "Use an HTTP or HTTPS URL (maximum 8192 bytes)"
    );
    Ok(())
}
pub fn parse(s: &str) -> Result<Command> {
    let mut words = s.split_whitespace();
    let c = words.next().unwrap_or("");
    let args: Vec<_> = words.collect();
    match (c, args.as_slice()) {
        ("u", [u]) => {
            validate_url(u)?;
            Ok(Command::Url(u.to_string()))
        }
        ("s", []) => Ok(Command::Stop),
        ("p", []) => Ok(Command::Play),
        ("v", [v]) => {
            let v = v
                .parse::<u8>()
                .ok()
                .filter(|v| *v <= 100)
                .ok_or_else(|| anyhow::anyhow!("Use v NUMBER (0–100)"))?;
            Ok(Command::Volume(v))
        }
        ("i", []) => Ok(Command::Info),
        ("h", []) => Ok(Command::Home),
        ("?", []) => Ok(Command::Help),
        _ => bail!("Use ? for commands"),
    }
}
pub fn authorized(account: Option<&str>, allow: &[String]) -> bool {
    account.is_some_and(|s| !s.trim().is_empty() && allow.iter().any(|a| a == s))
}
// Never disclose any path, query, fragment or userinfo: tokens can occur anywhere.
pub fn public_url(s: &str) -> String {
    url::Url::parse(s)
        .ok()
        .and_then(|u| u.host_str().map(|h| format!("{}://{}/…", u.scheme(), h)))
        .unwrap_or_else(|| "(invalid URL)".into())
}
pub fn status(s: &str) -> String {
    let mut end = s.len().min(511);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].chars().filter(|c| !c.is_control()).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands() {
        assert_eq!(parse("v 0").unwrap(), Command::Volume(0));
        for s in ["v 101", "v -1", "s x", "u file:///a", "u https://", "u"] {
            assert!(parse(s).is_err());
        }
        assert_eq!(
            parse("u https://a.test/live").unwrap(),
            Command::Url("https://a.test/live".into())
        );
    }
    #[test]
    fn identity() {
        let a = vec!["alice".into(), "".into()];
        assert!(!authorized(None, &a));
        assert!(!authorized(Some(""), &a));
        assert!(!authorized(Some("Alice"), &a));
        assert!(authorized(Some("alice"), &a));
    }
    #[test]
    fn privacy_unicode() {
        assert_eq!(
            public_url("https://a:secret@host/a/token?q=secret"),
            "https://host/…"
        );
        let s = status(&"🎵".repeat(200));
        assert!(s.len() <= 511);
        assert_eq!(s.chars().count(), 127);
    }
}
