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
        let (tx, pcm) = mpsc::channel(4);
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
    let (bytes_tx, bytes_rx) = mpsc::channel(8);
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
                for part in bytes.chunks(4096) {
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
