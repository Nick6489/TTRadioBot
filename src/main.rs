mod command;
mod config;
mod icy;
mod native;
mod queue;
mod state;
mod stream;

use anyhow::{Context, Result, ensure};
use command::Command;
use config::Config;
use state::{Actual, Desired, Occupancy};
use std::time::{Duration, Instant};
use teamtalk::{
    Client, Event,
    types::{AudioPreprocessor, ChannelId, EncryptionContext},
};

struct Radio {
    worker: Option<stream::Worker>,
    pending: Option<stream::Pcm>,
    id: i32,
    submitted_samples: u64,
    elapsed_ms: u32,
    last_progress: Instant,
    format: Option<(u32, usize)>,
    retry_at: Instant,
    backoff: u64,
    actual: Actual,
    title: String,
    status: String,
    status_command: i32,
    status_due: Instant,
}
impl Radio {
    fn new() -> Self {
        Self {
            worker: None,
            pending: None,
            id: 1,
            submitted_samples: 0,
            elapsed_ms: 0,
            last_progress: Instant::now(),
            format: None,
            retry_at: Instant::now(),
            backoff: 1,
            actual: Actual::Stopped,
            title: String::new(),
            status: String::new(),
            status_command: 0,
            status_due: Instant::now(),
        }
    }
    fn next_id(&mut self) {
        self.id = if self.id == i32::MAX { 1 } else { self.id + 1 };
    }
    fn clear(&mut self, c: &Client) -> Option<i32> {
        if let Some(w) = self.worker.take() {
            w.stop();
        }
        self.pending = None;
        let ended = if self.format.is_some() && c.my_channel_id().0 > 0 {
            self.next_id();
            native::end(c, self.id);
            Some(self.id)
        } else {
            None
        };
        self.next_id();
        self.submitted_samples = 0;
        self.elapsed_ms = 0;
        self.format = None;
        self.title.clear();
        self.status_due = Instant::now();
        ended
    }
    fn reset(&mut self, c: &Client) {
        let _ = self.clear(c);
        self.retry_at = Instant::now();
        self.backoff = 1;
    }
    fn fail(&mut self, c: &Client, max: u64) {
        let _ = self.clear(c);
        self.actual = Actual::Retrying;
        self.retry_at = Instant::now() + Duration::from_secs(self.backoff);
        self.backoff = (self.backoff * 2).min(max);
    }
    fn progress(&mut self, p: teamtalk::types::AudioInputProgress) {
        if p.stream_id == self.id {
            self.elapsed_ms = p.elapsed_ms;
            self.last_progress = Instant::now();
        }
    }
    fn tick(&mut self, c: &Client, cfg: &Config, d: &Desired, want: bool) {
        if !want {
            if self.worker.is_some() || self.actual == Actual::Retrying {
                self.reset(c);
            }
            self.actual = if d.enabled {
                Actual::Waiting
            } else {
                Actual::Stopped
            };
            self.title.clear();
        } else {
            if self.worker.is_none() && Instant::now() >= self.retry_at {
                self.worker = Some(stream::Worker::start(
                    d.url.clone().unwrap(),
                    Duration::from_secs(cfg.timeout_seconds),
                ));
                self.actual = Actual::Connecting;
                self.last_progress = Instant::now();
            }
            if self.worker.as_ref().is_some_and(|w| *w.done.borrow()) {
                self.fail(c, cfg.max_backoff_seconds);
            }
            if let Some(w) = &mut self.worker {
                self.title = w.title.borrow().clone();
            }
            if self.worker.is_some()
                && self.last_progress.elapsed() > Duration::from_secs(cfg.timeout_seconds)
            {
                tracing::warn!("Audio input stalled; restarting at live position");
                self.fail(c, cfg.max_backoff_seconds);
            }
            // Keep at most ~150 ms submitted but not consumed. Consumption events,
            // rather than wall-clock sleeps, replenish this budget.
            let output_rate = c
                .my_channel()
                .and_then(|ch| queue::output_rate(&ch.audio_codec));
            while self.worker.is_some()
                && output_rate.is_some_and(|rate| {
                    queue::needs_audio(self.submitted_samples, self.elapsed_ms, rate)
                })
            {
                let pcm = self
                    .pending
                    .take()
                    .or_else(|| self.worker.as_mut().and_then(|w| w.pcm.try_recv().ok()));
                let Some(mut pcm) = pcm else {
                    break;
                };
                let fmt = (pcm.rate, pcm.channels);
                if self.format.is_some_and(|f| f != fmt) {
                    self.next_id();
                    native::end(c, self.id);
                    self.next_id();
                    self.submitted_samples = 0;
                    self.elapsed_ms = 0;
                }
                self.format = Some(fmt);
                // Gain is applied only when submitting, so queued decoder frames use current volume.
                let mut scaled = stream::Pcm {
                    samples: pcm.samples.clone(),
                    rate: pcm.rate,
                    channels: pcm.channels,
                };
                stream::gain(&mut scaled.samples, d.volume);
                if !native::insert(c, &scaled, self.id) {
                    self.pending = Some(pcm);
                    break;
                }
                self.submitted_samples += queue::output_frames(
                    pcm.samples.len() / pcm.channels,
                    pcm.rate,
                    output_rate.unwrap(),
                );
                pcm.samples.clear();
                self.actual = Actual::Playing;
                // Reset exponential backoff only after sustained recovery.
                if self.elapsed_ms > 10_000 {
                    self.backoff = 1;
                }
            }
        }
        let status = command::status(
            if self.actual == Actual::Playing && !self.title.is_empty() {
                &self.title
            } else {
                match self.actual {
                    Actual::Stopped => "Stopped",
                    Actual::Waiting => "Waiting for listeners",
                    Actual::Connecting => "Connecting / buffering",
                    Actual::Playing => "Live radio",
                    Actual::Retrying => "Retrying stream",
                }
            },
        );
        if status != self.status && c.is_connected() && Instant::now() >= self.status_due {
            self.status_command = c.set_status_message(&status).0;
            if self.status_command > 0 {
                self.status = status;
            }
            self.status_due = Instant::now() + Duration::from_secs(1);
        }
    }
    fn command(
        &mut self,
        c: &Client,
        cfg: &Config,
        d: &mut Desired,
        text: &str,
        account: Option<&str>,
    ) -> String {
        let cmd = match command::parse(text) {
            Ok(c) => c,
            Err(e) => return e.to_string(),
        };
        if matches!(cmd, Command::Info) {
            return format!(
                "URL: {}; title: {}; volume: {}%; {}; actual: {}",
                d.url
                    .as_deref()
                    .map(command::public_url)
                    .unwrap_or_else(|| "none".into()),
                if self.title.is_empty() {
                    "none"
                } else {
                    &self.title
                },
                d.volume,
                if d.enabled { "enabled" } else { "stopped" },
                self.actual
            );
        }
        if matches!(cmd, Command::Help) {
            return "u URL: save/switch; s: stop; p: play saved URL; v 0–100: volume; i: info; h: home; ?: help".into();
        }
        if !command::authorized(account, &cfg.controllers) {
            tracing::warn!("Unauthorized control command rejected");
            return "Not authorized: your authenticated account must be allowlisted".into();
        }
        if matches!(cmd, Command::Home) {
            let id = c.join_channel_path(&cfg.channel, &cfg.channel_password);
            return if id.0 > 0 {
                "Home channel join requested"
            } else {
                "Home channel join could not be requested"
            }
            .into();
        }
        match d.apply(&cmd, &cfg.state_file) {
            Ok(restart) => {
                if restart {
                    self.reset(c);
                }
            }
            Err(error) => return error.to_string(),
        }
        "Saved and applied".into()
    }
}

fn tls(c: &Client, cfg: &Config) -> Result<()> {
    if cfg.tls {
        let ca = cfg
            .ca_file
            .clone()
            .unwrap_or_else(|| "/etc/ssl/certs/ca-certificates.crt".into());
        if !cfg.insecure_tls {
            ensure!(
                std::path::Path::new(&ca).is_file(),
                "TLS CA file missing; configure ca_file"
            );
        }
        ensure!(ca.len() < 512, "CA filename exceeds SDK limit");
        let ctx = EncryptionContext::new("", "", ca, "", !cfg.insecure_tls, false, 8);
        ensure!(
            c.set_encryption_context(&ctx),
            "SDK refused TLS configuration"
        );
        if cfg.insecure_tls {
            tracing::warn!("TeamTalk TLS certificate verification explicitly disabled");
        }
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    // Restrict dependency logging: HTTP and decoder diagnostics may contain URLs.
    tracing_subscriber::fmt()
        .with_env_filter("ttradiobot=info")
        .with_target(false)
        .init();
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: ttradiobot /path/to/config.toml");
        std::process::exit(2);
    }
    let cfg = Config::load(&args[1])?;
    let mut desired = Desired::load(&cfg.state_file, cfg.default_volume)?;
    teamtalk::init_with_path(&cfg.sdk_library).context("load configured TeamTalk SDK library")?;
    if let (Some(name), Some(key)) = (&cfg.license_name, &cfg.license_key) {
        ensure!(
            teamtalk::set_license(name, key)
                .map_err(|_| anyhow::anyhow!("SDK license setup failed"))?,
            "SDK rejected license information"
        );
    }
    let c = Client::new()?;
    tls(&c, &cfg)?;
    ensure!(
        c.set_audio_preprocessor(&AudioPreprocessor::None),
        "cannot disable audio preprocessing"
    );
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut radio = Radio::new();
    let mut occupancy = Occupancy::default();
    let mut ready = false;
    let mut login = 0;
    let mut join = 0;
    let mut connect_at = Instant::now();
    let mut connection_deadline: Option<Instant> = None;
    let mut connection_backoff = 1;
    let mut channel = ChannelId(0);
    let mut multipart = std::collections::HashSet::new();
    loop {
        tokio::select! {_=term.recv()=>break,_=&mut interrupt=>break,_=tick.tick()=>{}}
        let now = Instant::now();
        if connection_deadline.is_some_and(|t| now >= t) {
            tracing::warn!("TeamTalk synchronization timed out");
            let _ = c.disconnect();
            ready = false;
            radio.reset(&c);
            connection_deadline = None;
            connect_at = now + Duration::from_secs(connection_backoff);
            connection_backoff = (connection_backoff * 2).min(cfg.max_backoff_seconds);
        }
        if !c.is_connected() && connection_deadline.is_none() && now >= connect_at {
            tracing::info!("Connecting to TeamTalk");
            login = 0;
            join = 0;
            ready = false;
            if c.connect(&cfg.host, cfg.tcp_port, cfg.udp_port, cfg.tls)
                .is_ok()
            {
                connection_deadline = Some(now + Duration::from_secs(cfg.timeout_seconds));
            } else {
                connect_at = now + Duration::from_secs(connection_backoff);
                connection_backoff = (connection_backoff * 2).min(cfg.max_backoff_seconds);
            }
        }
        // Bound event work to preserve signal responsiveness even under server floods.
        for _ in 0..256 {
            let Some((event, msg)) = c.poll(0) else {
                break;
            };
            match event {
                Event::ConnectSuccess => {
                    tracing::info!("TeamTalk connected; logging in");
                    login = c
                        .login(&cfg.nickname, &cfg.username, &cfg.password, "TTRadioBot")
                        .0;
                    connection_deadline = Some(now + Duration::from_secs(cfg.timeout_seconds));
                }
                Event::CmdSuccess if msg.source() == login => {
                    join = c.join_channel_path(&cfg.channel, &cfg.channel_password).0;
                    connection_deadline = Some(now + Duration::from_secs(cfg.timeout_seconds));
                }
                Event::CmdSuccess if msg.source() == join => {
                    ready = true;
                    connection_deadline = None;
                    connection_backoff = 1;
                    channel = c.my_channel_id();
                    radio.status.clear();
                    tracing::info!("Channel membership synchronized");
                }
                Event::CmdError if msg.source() == login || msg.source() == join => {
                    tracing::warn!("TeamTalk login/channel command rejected");
                    let _ = c.disconnect();
                    ready = false;
                    radio.reset(&c);
                    connection_deadline = None;
                    connect_at = now + Duration::from_secs(connection_backoff);
                    connection_backoff = (connection_backoff * 2).min(cfg.max_backoff_seconds);
                }
                Event::ConnectionLost
                | Event::ConnectFailed
                | Event::ConnectCryptError
                | Event::MySelfLoggedOut
                | Event::MySelfKicked => {
                    tracing::warn!("TeamTalk disconnected or kicked; retry scheduled");
                    ready = false;
                    radio.reset(&c);
                    let _ = c.disconnect();
                    multipart.clear();
                    connection_deadline = None;
                    connect_at = now + Duration::from_secs(connection_backoff);
                    connection_backoff = (connection_backoff * 2).min(cfg.max_backoff_seconds);
                }
                Event::CmdError => {
                    if msg.source() == radio.status_command {
                        radio.status.clear();
                        radio.status_due = now + Duration::from_secs(5);
                        tracing::warn!(
                            "TeamTalk rejected status update; check account permissions/flood limits"
                        );
                    }
                }
                Event::AudioInput => {
                    if let Some(p) = msg.audio_input_progress() {
                        radio.progress(p);
                    }
                }
                Event::TextMessage => {
                    if let Some(m) = msg.text() {
                        use teamtalk_sys::TextMsgType;
                        let accepted = m.msg_type == TextMsgType::MSGTYPE_USER
                            || cfg.channel_commands
                                && m.msg_type == TextMsgType::MSGTYPE_CHANNEL
                                && m.channel_id == c.my_channel_id();
                        if accepted && m.from_id != c.my_id() {
                            let continuation = multipart.remove(&m.from_id);
                            if m.more {
                                multipart.insert(m.from_id);
                            }
                            let reply = if continuation || m.more || m.text.len() > 8192 {
                                "Send each command as one short message".into()
                            } else {
                                let sender = c.get_user(m.from_id);
                                radio.command(
                                    &c,
                                    &cfg,
                                    &mut desired,
                                    &m.text,
                                    sender.as_ref().map(|u| u.username.as_str()),
                                )
                            };
                            let _ = c.send_to_user(m.from_id, &reply);
                        }
                    }
                }
                _ => {}
            }
        }
        let current = c.my_channel_id();
        if current != channel {
            radio.reset(&c);
            occupancy = Occupancy::default();
            channel = current;
        }
        let listeners = ready
            && channel.0 > 0
            && c.get_channel_users(channel)
                .iter()
                .any(|u| u.id != c.my_id() && !cfg.excluded_listeners.contains(&u.username));
        let want = occupancy.wants_audio(
            &desired,
            ready && channel.0 > 0,
            listeners,
            radio.worker.is_some() || radio.actual == Actual::Retrying,
            now,
            Duration::from_secs(cfg.grace_seconds),
        );
        let previous = radio.actual;
        radio.tick(&c, &cfg, &desired, want);
        if radio.actual != previous {
            tracing::info!(state=%radio.actual,"Playback state changed");
        }
    }
    tracing::info!("Shutting down");
    if let Some(ended) = radio.clear(&c) {
        // Let the replacement session's end marker pass the encoder before
        // disconnect tears it down. The native queue has already been cleared.
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if let Some((Event::AudioInput, message)) = c.poll(10)
                && message
                    .audio_input_progress()
                    .is_some_and(|p| p.stream_id == ended && p.queue_ms == 0 && p.elapsed_ms == 0)
            {
                break;
            }
        }
    }
    let _ = c.disconnect();
    Ok(())
}
