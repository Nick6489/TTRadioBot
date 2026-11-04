//! Opt-in integration check against the isolated loopback fixture server.
//! No capture/playback device is initialized in the bot. The receiver uses the
//! SDK virtual output device only to collect decoded remote PCM.
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use teamtalk::{
    AudioBlockView, Client, Event,
    types::{ChannelId, SoundDeviceId, UserId},
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn pump(c: &Client, duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        let _ = c.poll(10);
    }
}
fn until(c: &Client, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(Instant::now() < deadline, "TeamTalk condition timed out");
        let _ = c.poll(10);
    }
}
fn request(c: &Client, bot: UserId, command: &str) -> String {
    let _ = c.send_to_user(bot, command);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "No command reply");
        if let Some((Event::TextMessage, m)) = c.poll(20)
            && let Some(t) = m.text()
            && t.from_id == bot
        {
            return t.text;
        }
    }
}
struct Http {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Http {
    fn new() -> Self {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", socket.local_addr().unwrap());
        socket.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut children = vec![];
            while !stopping.load(Ordering::SeqCst) {
                if let Ok((mut peer, _)) = socket.accept() {
                    let done = stopping.clone();
                    children.push(std::thread::spawn(move||{peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();peer.set_write_timeout(Some(Duration::from_secs(1))).unwrap();let mut req=vec![];let mut b=[0];while !req.ends_with(b"\r\n\r\n"){if peer.read(&mut b).unwrap_or(0)==0{return;}req.push(b[0]);}let title=if req.starts_with(b"GET /b "){"Test B"}else{"Test A"};if write!(peer,"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nicy-metaint: 128\r\nConnection: close\r\n\r\n").is_err(){return;}let tone=include_bytes!("fixtures/tone.mp3");let metadata=format!("StreamTitle='{title}';");let blocks=metadata.len().div_ceil(16);let mut pos=0;while !done.load(Ordering::SeqCst){let mut audio=vec![0;128];for v in &mut audio {*v=tone[pos%tone.len()];pos+=1;}audio.push(blocks as u8);audio.extend(metadata.as_bytes());audio.resize(129+blocks*16,0);if peer.write_all(&audio).is_err(){break;}std::thread::sleep(Duration::from_millis(5));}}));
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            for child in children {
                let _ = child.join();
            }
        });
        Self {
            url,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Http {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.thread.take().unwrap().join();
    }
}

#[test]
#[ignore = "requires TT_SDK_LIBRARY and loopback fixture TeamTalk server on port 19333"]
fn relay_controls_occupancy_and_shutdown() {
    let library = std::env::var("TT_SDK_LIBRARY").expect("TT_SDK_LIBRARY");
    teamtalk::init_with_path(&library).unwrap();
    let c = Client::new().unwrap();
    c.connect("127.0.0.1", 19333, 19333, false).unwrap();
    let mut logged = false;
    let deadline = Instant::now() + Duration::from_secs(8);
    while !logged {
        assert!(Instant::now() < deadline);
        if let Some((e, _)) = c.poll(20) {
            match e {
                Event::ConnectSuccess => {
                    let _ = c.login("Test listener", "controller", "test", "TTRadioBot tests");
                }
                Event::MySelfLoggedIn => logged = true,
                _ => {}
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"host = "127.0.0.1"
tcp_port = 19333
udp_port = 19333
tls = false
sdk_library = {library:?}
username = "radio"
password = "test"
nickname = "Radio test"
channel = "/"
controllers = ["controller"]
grace_seconds = 1
state_file = {state:?}
"#
        ),
    )
    .unwrap();
    let mut bot_process = Process(
        Command::new(env!("CARGO_BIN_EXE_ttradiobot"))
            .arg(&config)
            .stdout(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    until(&c, || {
        c.get_server_users()
            .iter()
            .any(|u| u.username == "radio" && u.channel_id.0 > 0)
    });
    let mut bot = c.get_user_by_username("radio").unwrap().id;
    let http = Http::new();
    assert_eq!(
        request(&c, bot, &format!("u {}/a", http.url)),
        "Saved and applied"
    );
    pump(&c, Duration::from_millis(200));
    assert!(request(&c, bot, "i").contains("actual: Waiting"));
    // Only the receiving test client opens an SDK virtual output device.
    assert!(c.init_sound_output_device(SoundDeviceId(
        teamtalk_sys::TT_SOUNDDEVICE_ID_TEAMTALK_VIRTUAL
    )));
    let energy = Arc::new(Mutex::new((0_u64, 0_usize)));
    let collect = energy.clone();
    // Upstream stream_audio_blocks filters by a User payload; AudioBlock events
    // carry their user only in nSource. Consume explicitly in this test receiver.
    let _subscription = c.on_event(Event::AudioBlock).subscribe(move |ctx| {
        if ctx.message().source() != bot.0 {
            return;
        }
        let client = ctx.client();
        let Some(ptr) =
            client.acquire_user_audio_block(teamtalk_sys::StreamType::STREAMTYPE_VOICE as u32, bot)
        else {
            return;
        };
        // SAFETY: acquired from this client, inspected only before exactly one
        // matching release on this event thread; no slice escapes the callback.
        unsafe {
            if let Some(b) = AudioBlockView::from_block(&*ptr) {
                let mut e = collect.lock().unwrap();
                e.0 += b
                    .data
                    .iter()
                    .map(|s| u64::from(s.unsigned_abs()))
                    .sum::<u64>();
                e.1 += b.data.len();
            }
            assert!(client.release_user_audio_block(ptr));
        }
    });
    let _ = c.join_channel(ChannelId(1), "");
    until(&c, || c.my_channel_id().0 == 1);
    assert!(c.enable_audio_block_event(
        bot,
        teamtalk_sys::StreamType::STREAMTYPE_VOICE as u32,
        true
    ));
    let _ = c.subscribe(
        bot,
        teamtalk::types::Subscriptions::from_raw(teamtalk::types::Subscriptions::VOICE),
    );
    until(&c, || {
        c.get_user(bot).is_some_and(|u| u.status_msg == "Test A")
    });
    pump(&c, Duration::from_secs(1));
    let baseline = {
        let e = *energy.lock().unwrap();
        assert!(e.0 > 0 && e.1 > 0, "received nonzero remote PCM");
        e.0 as f64 / e.1 as f64
    };
    assert_eq!(request(&c, bot, "v 0"), "Saved and applied");
    pump(&c, Duration::from_secs(1));
    *energy.lock().unwrap() = (0, 0);
    pump(&c, Duration::from_secs(1));
    let silent = *energy.lock().unwrap();
    assert!(silent.1 > 0);
    assert!(
        (silent.0 as f64 / silent.1 as f64) < baseline * 0.02,
        "zero gain is silent after codec tail"
    );
    assert_eq!(request(&c, bot, "v 50"), "Saved and applied");
    pump(&c, Duration::from_secs(1));
    *energy.lock().unwrap() = (0, 0);
    pump(&c, Duration::from_secs(1));
    let half = *energy.lock().unwrap();
    let ratio = (half.0 as f64 / half.1 as f64) / baseline;
    assert!((0.35..0.65).contains(&ratio), "half-volume ratio: {ratio}");
    // An administrator with the controller's display nickname has no implicit authority.
    let outsider = Client::new().unwrap();
    outsider.connect("127.0.0.1", 19333, 19333, false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline);
        if let Some((event, _)) = outsider.poll(20) {
            if event == Event::ConnectSuccess {
                let _ = outsider.login("controller", "outsider", "test", "TTRadioBot tests");
            }
            if event == Event::MySelfLoggedIn {
                break;
            }
        }
    }
    assert!(request(&outsider, bot, "s").starts_with("Not authorized"));
    assert!(request(&outsider, bot, "i").contains("volume: 50%"));
    let _ = outsider.disconnect();
    let _ = c.leave_channel();
    pump(&c, Duration::from_millis(300));
    let _ = c.join_channel(ChannelId(1), "");
    pump(&c, Duration::from_millis(400));
    assert!(
        request(&c, bot, "i").contains("actual: Playing"),
        "grace cancellation"
    );
    let _ = c.leave_channel();
    pump(&c, Duration::from_millis(1500));
    assert!(request(&c, bot, "i").contains("actual: Waiting"));
    let _ = c.join_channel(ChannelId(1), "");
    until(&c, || {
        c.get_user(bot).is_some_and(|u| u.status_msg == "Test A")
    });
    assert_eq!(request(&c, bot, "s"), "Saved and applied");
    let _ = c.leave_channel();
    pump(&c, Duration::from_millis(200));
    let _ = c.join_channel(ChannelId(1), "");
    pump(&c, Duration::from_millis(300));
    let info = request(&c, bot, "i");
    assert!(info.contains("stopped; actual: Stopped"));
    assert!(info.contains("title: none"));
    assert_eq!(request(&c, bot, "p"), "Saved and applied");
    until(&c, || {
        c.get_user(bot).is_some_and(|u| u.status_msg == "Test A")
    });
    assert_eq!(
        request(&c, bot, &format!("u {}/b", http.url)),
        "Saved and applied"
    );
    until(&c, || {
        c.get_user(bot).is_some_and(|u| u.status_msg == "Test B")
    });
    pump(&c, Duration::from_millis(300));
    assert!(request(&c, bot, "i").contains("title: Test B"));
    // A channel kick disconnects this bot and triggers login/join recovery.
    let previous_bot = bot;
    let _ = c.kick_user(bot, ChannelId(1));
    until(&c, || {
        c.get_user_by_username("radio")
            .is_some_and(|u| u.id != previous_bot && u.status_msg == "Test B")
    });
    bot = c.get_user_by_username("radio").unwrap().id;
    assert!(request(&c, bot, "i").contains("volume: 50%; enabled; actual: Playing"));
    assert!(
        Command::new("kill")
            .arg("-TERM")
            .arg(bot_process.0.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    let start = Instant::now();
    loop {
        if let Some(status) = bot_process.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(3));
        pump(&c, Duration::from_millis(20));
    }
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(saved["enabled"], true);
    pump(&c, Duration::from_millis(100));
    let previous_bot = bot;
    let mut restarted = Process(
        Command::new(env!("CARGO_BIN_EXE_ttradiobot"))
            .arg(&config)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    until(&c, || {
        c.get_user_by_username("radio")
            .is_some_and(|u| u.id != previous_bot && u.status_msg == "Test B")
    });
    bot = c.get_user_by_username("radio").unwrap().id;
    assert!(request(&c, bot, "i").contains("volume: 50%; enabled; actual: Playing"));
    assert!(
        Command::new("kill")
            .arg("-INT")
            .arg(restarted.0.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    let start = Instant::now();
    loop {
        if let Some(status) = restarted.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(3));
        pump(&c, Duration::from_millis(20));
    }
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(state).unwrap()).unwrap();
    assert_eq!(saved["enabled"], true);

    let _ = c.disconnect();
}
