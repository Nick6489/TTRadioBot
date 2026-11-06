use crate::icy::Icy;
use anyhow::{Result, ensure};
use futures_util::StreamExt;
use std::{
    io::{self, Read, Seek, SeekFrom},
    time::Duration,
};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::DecoderOptions,
    formats::FormatOptions,
    io::{MediaSource, MediaSourceStream},
    meta::MetadataOptions,
    probe::Hint,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
#[derive(Debug)]
pub struct Pcm {
    pub samples: Vec<i16>,
    pub rate: u32,
    pub channels: usize,
}
pub fn gain(samples: &mut [i16], volume: u8) {
    for s in samples {
        *s = (i32::from(*s) * i32::from(volume) / 100) as i16;
    }
}
pub struct Worker {
    pub pcm: mpsc::Receiver<Pcm>,
    pub title: watch::Receiver<String>,
    task: JoinHandle<()>,
    cancel: watch::Sender<bool>,
    pub done: watch::Receiver<bool>,
}
impl Worker {
    pub fn start(url: String, timeout: Duration) -> Self {
        // ~418 ms of 1152-sample MP3 frames at 44.1 kHz, bounded so a brief
        // source delay can be absorbed without accumulating stale radio audio.
        let (tx, pcm) = mpsc::channel(16);
        let (title_tx, title) = watch::channel(String::new());
        let (cancel, cancel_rx) = watch::channel(false);
        let (done_tx, done) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut cancel_rx = cancel_rx;
            tokio::select! {_ = cancel_rx.changed()=>{}, result=run(url,timeout,tx,title_tx)=> {if result.is_err(){tracing::warn!("Radio stream failed; retry scheduled");}}}
            let _ = done_tx.send(true);
        });
        Self {
            pcm,
            title,
            task,
            cancel,
            done,
        }
    }
    pub fn stop(self) {
        let _ = self.cancel.send(true);
        self.task.abort();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
        self.task.abort();
    }
}
struct Chunks {
    rx: mpsc::Receiver<Vec<u8>>,
    current: io::Cursor<Vec<u8>>,
}
impl Read for Chunks {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            let n = self.current.read(out)?;
            if n > 0 || out.is_empty() {
                return Ok(n);
            }
            match self.rx.blocking_recv() {
                Some(b) => self.current = io::Cursor::new(b),
                None => return Ok(0),
            }
        }
    }
}
struct Source(Icy<Chunks>);
impl Read for Source {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read(b)
    }
}
impl Seek for Source {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "live source"))
    }
}
impl MediaSource for Source {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}
async fn run(
    url: String,
    timeout: Duration,
    pcm: mpsc::Sender<Pcm>,
    title: watch::Sender<String>,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("TTRadioBot/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(timeout)
        .build()?;
    let response = tokio::time::timeout(
        timeout,
        client
            .get(url)
            .header("Icy-MetaData", "1")
            .header("Accept-Encoding", "identity")
            .send(),
    )
    .await??
    .error_for_status()?;
    let interval = match response.headers().get("icy-metaint") {
        Some(h) => {
            let n = h.to_str()?.parse::<usize>()?;
            ensure!((1..=16 * 1024 * 1024).contains(&n), "invalid ICY interval");
            Some(n)
        }
        None => None,
    };
    let station = response
        .headers()
        .get("icy-name")
        .and_then(|h| h.to_str().ok())
        .map(crate::command::status)
        .unwrap_or_else(|| "Live radio".into());
    title.send_replace(station.clone());
    let (bytes_tx, bytes_rx) = mpsc::channel(2);
    let source = Source(Icy::new(
        Chunks {
            rx: bytes_rx,
            current: io::Cursor::new(vec![]),
        },
        interval,
        move |s| {
            title.send_replace(if s.is_empty() { station.clone() } else { s });
        },
    ));
    let decode = tokio::task::spawn_blocking(move || decode(source, pcm));
    // Dropping this future closes byte input; dropping Worker closes PCM output.
    let mut body = response.bytes_stream();
    loop {
        match tokio::time::timeout(timeout, body.next()).await {
            Ok(Some(Ok(bytes))) => {
                for part in bytes.chunks(1024) {
                    if tokio::time::timeout(timeout, bytes_tx.send(part.to_vec()))
                        .await?
                        .is_err()
                    {
                        return Ok(());
                    }
                }
            }
            Ok(None) => break,
            _ => return Err(anyhow::anyhow!("stream read interrupted")),
        }
    }
    drop(bytes_tx);
    decode.await??;
    Ok(())
}
fn decode(source: Source, tx: mpsc::Sender<Pcm>) -> Result<()> {
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow::anyhow!("no audio track"))?;
    let id = track.id;
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let mut bad = 0;
    loop {
        let packet = format.next_packet()?;
        if packet.track_id() != id {
            continue;
        }
        let audio = match decoder.decode(&packet) {
            Ok(a) => {
                bad = 0;
                a
            }
            Err(symphonia::core::errors::Error::DecodeError(_)) => {
                bad += 1;
                ensure!(bad < 20, "too many corrupt frames");
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let spec = *audio.spec();
        let channels = spec.channels.count();
        ensure!(
            (1..=2).contains(&channels) && (8000..=192000).contains(&spec.rate),
            "unsupported audio format"
        );
        ensure!(audio.capacity() <= 16384, "oversized audio frame");
        let mut samples = SampleBuffer::<i16>::new(audio.capacity() as u64, spec);
        samples.copy_interleaved_ref(audio);
        if tx
            .blocking_send(Pcm {
                samples: samples.samples().to_vec(),
                rate: spec.rate,
                channels,
            })
            .is_err()
        {
            return Ok(());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn volume() {
        let original = [i16::MIN, -100, 0, 100, i16::MAX];
        let mut p = original;
        gain(&mut p, 100);
        assert_eq!(p, original);
        gain(&mut p, 50);
        assert_eq!(p, [-16384, -50, 0, 50, 16383]);
        gain(&mut p, 0);
        assert_eq!(p, [0; 5]);
    }
}

#[cfg(test)]
mod pipeline_tests {
    use super::*;
    use std::{
        io::Write,
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    fn server(
        station: &str,
        stall: bool,
    ) -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let station = station.to_string();
        let closed = Arc::new(AtomicBool::new(false));
        let observed = closed.clone();
        let thread = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut one = [0];
            while !request.ends_with(b"\r\n\r\n") {
                if socket.read(&mut one).unwrap_or(0) == 0 {
                    return;
                }
                request.push(one[0]);
            }
            assert!(
                String::from_utf8_lossy(&request)
                    .to_ascii_lowercase()
                    .contains("icy-metadata: 1")
            );
            assert!(String::from_utf8_lossy(&request).lines().any(|line| {
                line.eq_ignore_ascii_case(concat!(
                    "User-Agent: TTRadioBot/",
                    env!("CARGO_PKG_VERSION")
                ))
            }));
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nicy-name: {station}\r\nConnection: close\r\n\r\n").unwrap();
            if stall {
                let _ = socket.read(&mut one);
            } else {
                let tone = include_bytes!("../tests/fixtures/tone.mp3");
                for _ in 0..100 {
                    if socket.write_all(tone).is_err() {
                        break;
                    }
                }
            }
            observed.store(true, Ordering::SeqCst);
        });
        (format!("http://{address}/live"), closed, thread)
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn decode_bounded_and_cancel_switch() {
        let (url, _, thread) = server("Old station", false);
        let mut old = Worker::start(url, Duration::from_secs(2));
        let pcm = tokio::time::timeout(Duration::from_secs(3), old.pcm.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!((pcm.rate, pcm.channels), (44100, 2));
        assert_eq!(pcm.samples.len() % 2, 0);
        assert!(pcm.samples.iter().any(|s| *s != 0));
        let old_title = old.title.clone();
        assert_eq!(&*old_title.borrow(), "Old station");
        old.stop();
        let (url, closed, thread2) = server("New station", true);
        let mut new = Worker::start(url, Duration::from_secs(2));
        tokio::time::timeout(Duration::from_secs(3), new.title.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&*new.title.borrow(), "New station");
        assert_ne!(*new.title.borrow(), *old_title.borrow());
        let cancel = InstantForTest::now();
        new.stop();
        while !closed.load(Ordering::SeqCst) {
            assert!(cancel.elapsed() < Duration::from_secs(2));
            tokio::task::yield_now().await;
        }
        thread.join().unwrap();
        thread2.join().unwrap();
    }
    use std::time::Instant as InstantForTest;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stalled_read_finishes() {
        let (url, _, thread) = server("Stalled", true);
        let mut w = Worker::start(url, Duration::from_millis(100));
        tokio::time::timeout(Duration::from_secs(2), w.done.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(*w.done.borrow());
        w.stop();
        thread.join().unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "external network smoke test"]
    async fn hkc_live() {
        let mut w = Worker::start(
            "https://listen.hkcradio.com/hkc.mp3".into(),
            Duration::from_secs(10),
        );
        let p = tokio::time::timeout(Duration::from_secs(20), w.pcm.recv())
            .await
            .unwrap()
            .expect("live MP3 decoded");
        assert!(!p.samples.is_empty());
        assert!((1..=2).contains(&p.channels));
        w.stop();
    }
}
