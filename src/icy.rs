//! Stateful ICY demultiplexer. Length and metadata states survive every read boundary.
use std::io::{self, Read};
pub struct Icy<R> {
    inner: R,
    interval: Option<usize>,
    remaining: usize,
    pub title: Option<String>,
    on_title: Box<dyn FnMut(String) + Send>,
}
impl<R: Read> Icy<R> {
    pub fn new(
        inner: R,
        interval: Option<usize>,
        on_title: impl FnMut(String) + Send + 'static,
    ) -> Self {
        Self {
            inner,
            interval,
            remaining: interval.unwrap_or(usize::MAX),
            title: None,
            on_title: Box::new(on_title),
        }
    }
    fn metadata(&mut self) -> io::Result<()> {
        let mut length = [0];
        self.inner.read_exact(&mut length)?;
        let mut data = vec![0; length[0] as usize * 16];
        self.inner.read_exact(&mut data)?;
        let text = String::from_utf8_lossy(&data);
        if let Some(t) = text
            .split("StreamTitle='")
            .nth(1)
            .and_then(|s| s.split("';").next().filter(|_| s.contains("';")))
        {
            let t = crate::command::status(t);
            if self.title.as_ref() != Some(&t) {
                self.title = Some(t.clone());
                (self.on_title)(t);
            }
        }
        self.remaining = self.interval.unwrap();
        Ok(())
    }
}
impl<R: Read> Read for Icy<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.interval.is_some() && self.remaining == 0 {
            self.metadata()?;
        }
        let n = out.len().min(self.remaining);
        let got = self.inner.read(&mut out[..n])?;
        self.remaining -= got;
        Ok(got)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    struct Split {
        data: io::Cursor<Vec<u8>>,
        chunk: usize,
    }
    impl Read for Split {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            let n = b.len().min(self.chunk);
            self.data.read(&mut b[..n])
        }
    }
    #[test]
    fn boundaries() {
        let mut wire = b"abc".to_vec();
        let title = "StreamTitle='🎵';".as_bytes();
        let len = title.len().div_ceil(16);
        wire.push(len as u8);
        wire.extend(title);
        wire.resize(4 + len * 16, 0);
        wire.extend(b"def\0ghi");
        for chunk in 1..wire.len() {
            let titles = Arc::new(Mutex::new(vec![]));
            let t = titles.clone();
            let mut r = Icy::new(
                Split {
                    data: io::Cursor::new(wire.clone()),
                    chunk,
                },
                Some(3),
                move |s| t.lock().unwrap().push(s),
            );
            let mut out = vec![];
            r.read_to_end(&mut out).unwrap_err();
            assert_eq!(out, b"abcdefghi");
            assert_eq!(*titles.lock().unwrap(), vec!["🎵"]);
        }
    }
    #[test]
    fn truncated_and_malformed() {
        for data in [
            b"abc\x01short".to_vec(),
            b"abc\x02StreamTitle='broken".to_vec(),
        ] {
            let mut r = Icy::new(io::Cursor::new(data), Some(3), |_| panic!());
            assert!(r.read_to_end(&mut vec![]).is_err());
        }
        let mut r = Icy::new(io::Cursor::new(b"abc"), None, |_| panic!());
        let mut out = vec![];
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abc");
    }
}
