//! Playback backend shared by desktop platforms. Only decoded samples reach
//! the audio callback. HTTP and decoding run on the playback worker.
use std::{
    io::{self, BufReader, Read, Seek, SeekFrom},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use reqwest::{
    StatusCode,
    blocking::{Client, Response},
    header::{CONTENT_RANGE, RANGE},
};
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, Source};

pub enum MediaEvent {
    Opened,
    Ended,
    Failed(String),
    SeekFailed,
}

#[derive(Default)]
struct State {
    sink: Mutex<Option<Arc<Sink>>>,
    stopped: AtomicBool,
    playing: AtomicBool,
    volume: AtomicU32,
    position: Arc<AtomicU64>,
    duration: AtomicU64,
    seek: AtomicU64,
    device_failed: AtomicBool,
    recovering: AtomicBool,
}

const NO_SEEK: u64 = u64::MAX;
const PAGE: u64 = 512 * 1024;
const BUFFER_TARGET_US: u64 = 1_000_000;

impl State {
    fn sink(&self) -> Option<Arc<Sink>> {
        self.sink
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn make_sink(&self, mixer: &rodio::mixer::Mixer) -> Arc<Sink> {
        let sink = Arc::new(Sink::connect_new(mixer));
        let mut current = self.sink.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(old) = current.replace(sink.clone()) {
            old.stop();
        }
        if !self.playing.load(Relaxed) {
            sink.pause();
        }
        sink.set_volume(f32::from_bits(self.volume.load(Relaxed)));
        sink
    }
}

pub struct Media {
    state: Arc<State>,
    events: Receiver<MediaEvent>,
}

impl Media {
    pub fn open(address: &str, volume: f32) -> io::Result<Self> {
        let url = url::Url::parse(address).map_err(io::Error::other)?;
        if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a local audio relay",
            ));
        }
        let state = Arc::new(State {
            volume: AtomicU32::new(volume.to_bits()),
            seek: AtomicU64::new(NO_SEEK),
            ..State::default()
        });
        let (sender, events) = mpsc::channel();
        let worker = state.clone();
        thread::Builder::new()
            .name("fnmusic-audio".into())
            .spawn(move || {
                let result = play(url, &worker, &sender);
                if let Err(error) = result
                    && !worker.stopped.load(Relaxed)
                {
                    let _ = sender.send(MediaEvent::Failed(error.to_string()));
                }
            })?;
        Ok(Self { state, events })
    }

    pub fn event(&self) -> Option<MediaEvent> {
        self.events.try_recv().ok()
    }

    pub fn set_playing(&self, playing: bool) -> io::Result<()> {
        self.state.playing.store(playing, Relaxed);
        if let Some(sink) = self.state.sink() {
            if playing {
                sink.play();
            } else {
                sink.pause();
            }
        }
        Ok(())
    }

    pub fn seek(&self, ms: u64) -> io::Result<()> {
        self.state.seek.store(ms.min(NO_SEEK - 1), Relaxed);
        Ok(())
    }

    pub fn position(&self) -> Option<u64> {
        (self.state.sink().is_some() && self.state.seek.load(Relaxed) == NO_SEEK)
            .then(|| self.state.position.load(Relaxed) / 1000)
    }

    pub fn duration(&self) -> Option<u64> {
        let ms = self.state.duration.load(Relaxed);
        (ms > 0).then_some(ms)
    }

    pub fn buffering(&self) -> bool {
        self.state.recovering.load(Relaxed)
            || self.state.device_failed.load(Relaxed)
            || self.state.seek.load(Relaxed) != NO_SEEK
            || self.state.sink().is_none_or(|sink| sink.empty())
    }

    pub fn is_playing(&self) -> bool {
        self.state.playing.load(Relaxed) && !self.buffering()
    }

    pub fn volume(&self, volume: f32) -> io::Result<()> {
        self.state.volume.store(volume.to_bits(), Relaxed);
        if let Some(sink) = self.state.sink() {
            sink.set_volume(volume);
        }
        Ok(())
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        self.state.stopped.store(true, Relaxed);
        if let Some(sink) = self.state.sink() {
            sink.stop();
        }
    }
}

fn open_output(state: &Arc<State>) -> io::Result<OutputStream> {
    let callback_state = state.clone();
    OutputStreamBuilder::from_default_device()
        .map_err(|error| io::Error::other(format!("获取音频设备失败：{error}")))?
        .with_error_callback(move |_| {
            callback_state.device_failed.store(true, Relaxed);
        })
        .open_stream_or_fallback()
        .map_err(|error| io::Error::other(format!("打开音频设备失败：{error}")))
}

fn recover_output(state: &Arc<State>) -> io::Result<OutputStream> {
    let mut last_error = None;
    // Allow USB/Bluetooth devices and Windows' default endpoint to settle.
    for _ in 0..30 {
        if state.stopped.load(Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "播放已停止"));
        }
        match open_output(state) {
            Ok(output) => return Ok(output),
            Err(error) => last_error = Some(error),
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(io::Error::other(format!(
        "音频设备重连失败，请检查系统默认输出设备：{}",
        last_error.unwrap()
    )))
}

fn play(url: url::Url, state: &Arc<State>, events: &Sender<MediaEvent>) -> io::Result<()> {
    let failed = Arc::new(AtomicBool::new(false));
    let input = HttpAudio::open(url, failed.clone())
        .map_err(|error| io::Error::other(format!("读取音频失败：{error}")))?;
    let length = input.length;
    let seekable = input.sequential.is_none();
    let mut builder = Decoder::builder().with_data(BufReader::with_capacity(64 * 1024, input));
    if let Some(length) = length {
        builder = builder.with_byte_len(length);
    }
    builder = builder.with_seekable(seekable);
    let mut decoder = builder
        .build()
        .map_err(|error| io::Error::other(format!("音频解码失败：{error}")))?;
    if let Some(duration) = decoder.total_duration() {
        state.duration.store(duration.as_millis() as u64, Relaxed);
    }
    let mut output = open_output(state)?;
    let mut sink = state.make_sink(output.mixer());
    let _ = events.send(MediaEvent::Opened);
    let mut next_us = 0;
    let mut eof = false;
    let mut recoveries = 0;
    let mut last_recovery = Instant::now();
    while !state.stopped.load(Relaxed) {
        if state.device_failed.swap(false, Relaxed) {
            if last_recovery.elapsed() >= Duration::from_secs(5) {
                recoveries = 0;
            }
            if recoveries >= 3 {
                return Err(io::Error::other(
                    "音频输出设备持续失效，请在 Windows 声音设置中重新选择输出设备后重试",
                ));
            }
            recoveries += 1;
            last_recovery = Instant::now();
            state.recovering.store(true, Relaxed);
            sink.stop();
            // Dispose of the invalid endpoint before opening the current default.
            drop(output);
            let resume_us = state.position.load(Relaxed);
            state.device_failed.store(false, Relaxed);
            output = recover_output(state)?;
            if state.stopped.load(Relaxed) {
                return Ok(());
            }
            if seekable && decoder.try_seek(Duration::from_micros(resume_us)).is_ok() {
                next_us = resume_us;
                eof = false;
            }
            state.position.store(next_us, Relaxed);
            sink = state.make_sink(output.mixer());
            state.recovering.store(false, Relaxed);
        }
        let request = state.seek.load(Relaxed);
        if request != NO_SEEK {
            match seekable.then(|| decoder.try_seek(Duration::from_millis(request))) {
                Some(Ok(())) => {
                    next_us = request.saturating_mul(1000);
                    sink = state.make_sink(output.mixer());
                    state.position.store(next_us, Relaxed);
                    eof = false;
                }
                _ => {
                    let _ = events.send(MediaEvent::SeekFailed);
                }
            }
            let _ = state
                .seek
                .compare_exchange(request, NO_SEEK, Relaxed, Relaxed);
        }
        if eof && sink.empty() {
            let _ = events.send(MediaEvent::Ended);
            return Ok(());
        }
        if !eof
            && sink.len() < 96
            && (sink.empty()
                || next_us.saturating_sub(state.position.load(Relaxed)) < BUFFER_TARGET_US)
        {
            let channels = decoder.channels();
            let rate = decoder.sample_rate();
            if channels == 0 || rate == 0 {
                return Err(io::Error::other("invalid decoded audio format"));
            }
            let count = usize::from(channels) * (rate as usize / 10).max(1);
            let span = decoder
                .current_span_len()
                .filter(|n| *n > 0)
                .unwrap_or(count);
            let samples: Vec<_> = decoder.by_ref().take(count.min(span)).collect();
            if failed.load(Relaxed) {
                return Err(io::Error::other("NAS audio stream interrupted"));
            }
            if samples.is_empty() {
                eof = true;
            } else if !state.stopped.load(Relaxed) {
                let samples_per_second = u64::from(channels) * u64::from(rate);
                let length_us = samples.len() as u64 * 1_000_000 / samples_per_second;
                sink.append(PlayedChunk {
                    data: samples.into_iter(),
                    position: state.position.clone(),
                    channels,
                    rate,
                    base_us: next_us,
                    consumed: 0,
                });
                next_us += length_us;
            }
        } else {
            thread::sleep(Duration::from_millis(8));
        }
    }
    Ok(())
}

struct PlayedChunk {
    data: std::vec::IntoIter<f32>,
    position: Arc<AtomicU64>,
    channels: u16,
    rate: u32,
    base_us: u64,
    consumed: u64,
}

impl Iterator for PlayedChunk {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let sample = self.data.next()?;
        self.consumed += 1;
        if self.consumed == 1 || self.consumed % 256 == 0 || self.data.len() == 0 {
            let frequency = u64::from(self.channels) * u64::from(self.rate);
            self.position.store(
                self.base_us + self.consumed * 1_000_000 / frequency,
                Relaxed,
            );
        }
        Some(sample)
    }
}

impl Source for PlayedChunk {
    fn current_span_len(&self) -> Option<usize> {
        Some(self.data.len())
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

struct HttpAudio {
    client: Client,
    url: url::Url,
    length: Option<u64>,
    position: u64,
    page_start: u64,
    bytes: Vec<u8>,
    sequential: Option<Response>,
    failed: Arc<AtomicBool>,
}

impl HttpAudio {
    fn open(url: url::Url, failed: Arc<AtomicBool>) -> io::Result<Self> {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(None)
            .build()
            .map_err(io::Error::other)?;
        let mut input = Self {
            client,
            url,
            length: None,
            position: 0,
            page_start: 0,
            bytes: Vec::new(),
            sequential: None,
            failed,
        };
        input.load_page(0)?;
        Ok(input)
    }

    fn load_page(&mut self, start: u64) -> io::Result<()> {
        let mut result = self.fetch_page(start);
        for delay in [200, 600] {
            if !result.as_ref().is_err_and(|error| {
                matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                )
            }) {
                break;
            }
            thread::sleep(Duration::from_millis(delay));
            result = self.fetch_page(start);
        }
        result
    }

    fn fetch_page(&mut self, start: u64) -> io::Result<()> {
        let response = self
            .client
            .get(self.url.clone())
            .header(
                RANGE,
                format!("bytes={start}-{}", start.saturating_add(PAGE - 1)),
            )
            .send()
            .map_err(|error| {
                let kind = if error.is_timeout() {
                    io::ErrorKind::TimedOut
                } else {
                    io::ErrorKind::ConnectionReset
                };
                let error = error.without_url();
                let mut message = format!("连接音频服务失败：{error}");
                let mut cause = std::error::Error::source(&error);
                while let Some(error) = cause {
                    message.push_str(&format!("；{error}"));
                    cause = error.source();
                }
                io::Error::new(kind, message)
            })?;
        if start == 0 && self.length.is_none() && response.status() == StatusCode::OK {
            self.length = response.content_length();
            self.sequential = Some(response);
            return Ok(());
        }
        if response.status() != StatusCode::PARTIAL_CONTENT {
            let status = response.status();
            let reason = match status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "音频访问未授权，请重新登录",
                StatusCode::NOT_FOUND => "歌曲文件不存在或播放地址已失效",
                StatusCode::BAD_GATEWAY | StatusCode::GATEWAY_TIMEOUT => "NAS 音频服务暂时不可用",
                _ => "音频请求失败",
            };
            return Err(io::Error::new(
                if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
                    io::ErrorKind::Interrupted
                } else {
                    io::ErrorKind::Other
                },
                format!("{reason}（HTTP {status}）"),
            ));
        }
        let range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("bytes "))
            .and_then(|value| value.split_once('/'));
        let (offset, end, length) = range
            .and_then(|(span, length)| {
                let (offset, end) = span.split_once('-')?;
                Some((
                    offset.parse::<u64>().ok()?,
                    end.parse::<u64>().ok()?,
                    length.parse::<u64>().ok()?,
                ))
            })
            .ok_or_else(|| io::Error::other("invalid audio Content-Range"))?;
        if offset != start
            || end < start
            || end >= length
            || end - start >= PAGE
            || self.length.is_some_and(|known| known != length)
        {
            return Err(io::Error::other("mismatched audio Content-Range"));
        }
        let count = end - start + 1;
        let mut bytes = Vec::with_capacity(count as usize);
        response.take(count + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != count {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete audio range",
            ));
        }
        self.length = Some(length);
        self.page_start = start;
        self.bytes = bytes;
        Ok(())
    }

    fn read_audio(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.length.is_some_and(|len| self.position >= len) {
            return Ok(0);
        }
        let n = if let Some(response) = &mut self.sequential {
            response.read(out)?
        } else {
            if self.position < self.page_start
                || self.position - self.page_start >= self.bytes.len() as u64
            {
                self.load_page(self.position)?;
            }
            let start = (self.position - self.page_start) as usize;
            let n = out.len().min(self.bytes.len() - start);
            out[..n].copy_from_slice(&self.bytes[start..start + n]);
            n
        };
        self.position += n as u64;
        Ok(n)
    }
}

impl Read for HttpAudio {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let result = self.read_audio(out);
        if result.is_err() {
            self.failed.store(true, Relaxed);
        }
        result
    }
}

impl Seek for HttpAudio {
    fn seek(&mut self, offset: SeekFrom) -> io::Result<u64> {
        let position = match offset {
            SeekFrom::Start(value) => Some(value),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
            SeekFrom::End(delta) => self.length.and_then(|len| len.checked_add_signed(delta)),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "invalid audio seek"))?;
        if self.sequential.is_some() && position != self.position {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "server does not support byte ranges",
            ));
        }
        self.position = position;
        Ok(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::State, http::HeaderMap, response::IntoResponse, routing::get};
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn transient_audio_failure_retries_but_unauthorized_does_not() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for status in [StatusCode::BAD_GATEWAY, StatusCode::UNAUTHORIZED] {
            let requests = Arc::new(AtomicUsize::new(0));
            let count = requests.clone();
            let address = runtime.block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let app = Router::new().route(
                    "/track.wav",
                    get(move || {
                        let count = count.clone();
                        async move {
                            if count.fetch_add(1, Relaxed) == 0 {
                                status.into_response()
                            } else {
                                (
                                    StatusCode::PARTIAL_CONTENT,
                                    [(CONTENT_RANGE, "bytes 0-3/4")],
                                    b"RIFF".to_vec(),
                                )
                                    .into_response()
                            }
                        }
                    }),
                );
                tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
                format!("http://{address}/track.wav").parse().unwrap()
            });
            let result = HttpAudio::open(address, Arc::new(AtomicBool::new(false)));
            if status == StatusCode::BAD_GATEWAY {
                let mut input = result.unwrap();
                let mut bytes = Vec::new();
                input.read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, b"RIFF");
                assert_eq!(requests.load(Relaxed), 2);
            } else {
                let error = result.err().expect("unauthorized request must fail");
                assert!(error.to_string().contains("401"));
                assert_eq!(requests.load(Relaxed), 1);
            }
        }
    }

    #[test]
    fn ranged_wav_decodes_and_seeks_without_an_audio_device() {
        let (_runtime, addressed) = wav_server();
        let input = HttpAudio::open(addressed, Arc::new(AtomicBool::new(false))).unwrap();
        let length = input.length.unwrap();
        let mut decoder = Decoder::builder()
            .with_data(BufReader::new(input))
            .with_byte_len(length)
            .with_seekable(true)
            .build()
            .unwrap();
        assert_eq!(decoder.next(), Some(0.25));
        decoder.try_seek(Duration::from_secs(7)).unwrap();
        assert_eq!(decoder.next(), Some(-0.25));
        decoder.try_seek(Duration::from_secs(2)).unwrap();
        assert_eq!(decoder.next(), Some(0.25));
    }

    #[test]
    #[ignore = "requires an available native audio output device"]
    fn output_device_failure_recovers_at_playback_position() {
        let (_runtime, address) = wav_server();
        let media = Media::open(address.as_str(), 0.0).unwrap();
        media.set_playing(true).unwrap();
        let wait = |ready: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(8);
            while !ready() {
                if let Some(MediaEvent::Failed(reason)) = media.event() {
                    panic!("playback failed: {reason}");
                }
                assert!(Instant::now() < deadline, "playback timed out");
                thread::sleep(Duration::from_millis(10));
            }
        };
        wait(&|| media.position().is_some_and(|position| position >= 200));
        media.seek(2_000).unwrap();
        wait(&|| media.position().is_some_and(|position| position >= 2_000));
        let old_sink = media.state.sink().unwrap();
        media.state.device_failed.store(true, Relaxed);
        wait(&|| {
            media
                .state
                .sink()
                .is_some_and(|sink| !Arc::ptr_eq(&old_sink, &sink))
                && !media.buffering()
                && media
                    .position()
                    .is_some_and(|position| position >= 2_200 && position < 4_000)
        });
        assert_eq!(media.state.seek.load(Relaxed), NO_SEEK);
        assert!(media.state.playing.load(Relaxed));
        assert_eq!(media.state.volume.load(Relaxed), 0.0_f32.to_bits());
    }

    fn wav_server() -> (tokio::runtime::Runtime, url::Url) {
        const RATE: usize = 48_000;
        const FRAMES: usize = RATE * 9;
        let pcm_len = (FRAMES * 2) as u32;
        let mut wav = b"RIFF".to_vec();
        wav.extend((36 + pcm_len).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16_u32.to_le_bytes());
        wav.extend(1_u16.to_le_bytes()); // PCM
        wav.extend(1_u16.to_le_bytes()); // Mono
        wav.extend((RATE as u32).to_le_bytes());
        wav.extend((RATE as u32 * 2).to_le_bytes());
        wav.extend(2_u16.to_le_bytes());
        wav.extend(16_u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(pcm_len.to_le_bytes());
        for frame in 0..FRAMES {
            let sample = if frame < RATE * 5 {
                8192_i16
            } else {
                -8192_i16
            };
            wav.extend(sample.to_le_bytes());
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let addressed = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = Router::new()
                .route(
                    "/track.wav",
                    get(
                        |State(wav): State<Arc<Vec<u8>>>, headers: HeaderMap| async move {
                            let range = headers[RANGE]
                                .to_str()
                                .unwrap()
                                .strip_prefix("bytes=")
                                .unwrap();
                            let (first, last) = range.split_once('-').unwrap();
                            let first: usize = first.parse().unwrap();
                            let last = last.parse::<usize>().unwrap().min(wav.len() - 1);
                            (
                                StatusCode::PARTIAL_CONTENT,
                                [(CONTENT_RANGE, format!("bytes {first}-{last}/{}", wav.len()))],
                                wav[first..=last].to_vec(),
                            )
                                .into_response()
                        },
                    ),
                )
                .with_state(Arc::new(wav));
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            format!("http://{address}/track.wav").parse().unwrap()
        });
        (runtime, addressed)
    }
}
