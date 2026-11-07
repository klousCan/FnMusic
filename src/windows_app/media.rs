use std::sync::mpsc::{self, Receiver};

use windows::{
    Foundation::{TimeSpan, TypedEventHandler, Uri},
    Media::{
        Core::MediaSource,
        Playback::{MediaPlaybackSession, MediaPlaybackState, MediaPlayer},
    },
    Win32::System::WinRT::{RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize},
    core::{HSTRING, Result},
};

pub struct Apartment;

impl Apartment {
    pub fn new() -> Result<Self> {
        // The main winit window uses a single-threaded COM apartment on Windows.
        unsafe {
            RoInitialize(RO_INIT_SINGLETHREADED)?;
        }
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

pub enum MediaEvent {
    Opened,
    Ended,
    Failed,
}

pub struct Media {
    player: MediaPlayer,
    session: MediaPlaybackSession,
    events: Receiver<MediaEvent>,
}

impl Media {
    pub fn open(address: &str, volume: f32) -> Result<Self> {
        let player = MediaPlayer::new()?;
        let session = player.PlaybackSession()?;
        let (send, events) = mpsc::channel();
        let opened = send.clone();
        let ended = send.clone();
        player.MediaOpened(&TypedEventHandler::new(move |_, _| {
            let _ = opened.send(MediaEvent::Opened);
            Ok(())
        }))?;
        player.MediaEnded(&TypedEventHandler::new(move |_, _| {
            let _ = ended.send(MediaEvent::Ended);
            Ok(())
        }))?;
        player.MediaFailed(&TypedEventHandler::new(move |_, _| {
            let _ = send.send(MediaEvent::Failed);
            Ok(())
        }))?;
        player.SetAutoPlay(false)?;
        player.CommandManager()?.SetIsEnabled(false)?;
        player.SetVolume(f64::from(volume))?;
        let uri = Uri::CreateUri(&HSTRING::from(address))?;
        let source = MediaSource::CreateFromUri(&uri)?;
        player.SetSource(&source)?;
        Ok(Self {
            player,
            session,
            events,
        })
    }

    pub fn event(&self) -> Option<MediaEvent> {
        self.events.try_recv().ok()
    }

    pub fn set_playing(&self, playing: bool) -> Result<()> {
        if playing {
            self.player.Play()
        } else {
            self.player.Pause()
        }
    }

    pub fn seek(&self, ms: u64) -> Result<()> {
        self.session.SetPosition(TimeSpan {
            Duration: ms.min(i64::MAX as u64 / 10_000) as i64 * 10_000,
        })
    }

    pub fn position(&self) -> Option<u64> {
        self.session.Position().ok().map(milliseconds)
    }

    pub fn duration(&self) -> Option<u64> {
        self.session
            .NaturalDuration()
            .ok()
            .map(milliseconds)
            .filter(|&ms| ms > 0)
    }

    pub fn buffering(&self) -> bool {
        self.session.PlaybackState().is_ok_and(|state| {
            state == MediaPlaybackState::Opening || state == MediaPlaybackState::Buffering
        })
    }

    pub fn volume(&self, volume: f32) -> Result<()> {
        self.player.SetVolume(f64::from(volume))
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        let _ = self.player.Close();
    }
}

fn milliseconds(span: TimeSpan) -> u64 {
    span.Duration.max(0) as u64 / 10_000
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        thread,
        time::{Duration, Instant},
    };

    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{HeaderMap, Method, StatusCode, header},
        response::Response,
        routing::get,
    };
    use fnmusic::{
        api::{self, Api, AuthFuture, Authorization, Credentials, Session, SessionHooks},
        library::{self, Page},
        relay::Relay,
    };
    use tokio::{net::TcpListener, runtime::Runtime};
    use windows::Win32::System::WinRT::RO_INIT_MULTITHREADED;

    use super::*;

    #[test]
    #[ignore = "requires Windows audio and FNMUSIC_AUDIO_FIXTURES with sample WAV, MP3, M4A and FLAC"]
    fn windows_streams_and_seeks_supported_formats() {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED).unwrap();
        }
        let _apartment = Apartment;
        let root = PathBuf::from(std::env::var_os("FNMUSIC_AUDIO_FIXTURES").unwrap());
        let runtime = Runtime::new().unwrap();
        for (name, mime) in [
            ("wav", "audio/wav"),
            ("mp3", "audio/mpeg"),
            ("m4a", "audio/mp4"),
            ("flac", "audio/flac"),
        ] {
            let data = std::fs::read(root.join(format!("sample.{name}"))).unwrap();
            let (address, server) = runtime.block_on(async {
                let app = Router::new()
                    .route("/sample/{file}", get(fixture_audio))
                    .with_state((data, mime));
                let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = server.local_addr().unwrap();
                (
                    format!("http://{address}/sample/tone.{name}"),
                    tokio::spawn(async move { axum::serve(server, app).await.unwrap() }),
                )
            });
            verify_playback(&address, name);
            server.abort();
        }
    }

    struct ReadOnlySession;

    impl SessionHooks for ReadOnlySession {
        fn authorize(&self, _: Authorization, _: String, _: bool) -> AuthFuture {
            Box::pin(async { Err(api::Error::Expired) })
        }

        fn save(&self, _: &Credentials) -> api::Result<()> {
            Ok(())
        }
    }

    #[test]
    #[ignore = "requires a signed-in NAS session; prints artwork metadata only"]
    fn inspect_playlist_artwork() {
        let root = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap()).join("FnMusic");
        let credentials = super::super::vault::acceptance_credentials(&root).unwrap();
        let session = Session::new(Api::new().unwrap(), credentials, Box::new(ReadOnlySession));
        Runtime::new().unwrap().block_on(async {
            let playlists = library::load(&session, &Page::Playlists, 1).await.unwrap();
            let id = &playlists.playlists[0].id;
            let data = session.json::<serde_json::Value>(reqwest::Method::GET, "/api/v1/track/playlist-detail/list", &[("playlistGUID", id), ("page", "1"), ("size", "2")], None).await.unwrap().unwrap();
            for item in data["list"].as_array().unwrap() {
                let raw = item["coverId"].as_str().unwrap();
                for candidate in [raw.to_owned(), format!("album_{raw}")] {
                    println!("COVER {} {:?}", candidate, session.cover(&candidate, "160").await.map(|bytes| bytes.len()));
                }
                println!("ARTWORK {}", serde_json::json!({"keys":item.as_object().unwrap().keys().collect::<Vec<_>>(), "coverId":item["coverId"], "album":item["album"]}));
            }
        });
    }

    #[test]
    #[ignore = "requires a signed-in NAS session in FNMUSIC_ACCEPTANCE_DATA_DIR"]
    fn real_nas_flac() {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED).unwrap();
        }
        let _apartment = Apartment;
        let root = PathBuf::from(std::env::var_os("FNMUSIC_ACCEPTANCE_DATA_DIR").unwrap());
        let credentials = super::super::vault::acceptance_credentials(&root).unwrap();
        let session = std::sync::Arc::new(Session::new(
            Api::new().unwrap(),
            credentials,
            Box::new(ReadOnlySession),
        ));
        let runtime = Runtime::new().unwrap();
        let relay = Relay::start(&runtime).unwrap();
        let track = runtime
            .block_on(async {
                session.validate().await.unwrap();
                for number in 1..=20 {
                    let page = library::load(&session, &Page::Tracks, number)
                        .await
                        .unwrap();
                    if let Some(track) = page
                        .tracks
                        .into_iter()
                        .find(|track| track.format == "FLAC" && track.duration_ms >= 6_000)
                    {
                        return Some(track);
                    }
                    if !page.has_more {
                        break;
                    }
                }
                None
            })
            .expect("no FLAC longer than six seconds in the first 2,000 tracks");
        let address = relay.issue(session, track.id, &track.format, None);
        verify_playback(&address, "NAS FLAC");
    }

    fn verify_playback(address: &str, name: &str) {
        let media = Media::open(address, 0.0).unwrap();
        wait_until(name, &media, |event| {
            matches!(event, Some(MediaEvent::Opened))
        });
        media.set_playing(true).unwrap();
        wait_until(name, &media, |_| {
            media.position().is_some_and(|ms| ms > 200)
        });
        media.seek(4_000).unwrap();
        wait_until(name, &media, |_| {
            media.position().is_some_and(|ms| ms >= 3_900)
        });
        media.set_playing(false).unwrap();
    }

    fn wait_until(name: &str, media: &Media, mut ready: impl FnMut(Option<MediaEvent>) -> bool) {
        let start = Instant::now();
        loop {
            let event = media.event();
            assert!(
                !matches!(event, Some(MediaEvent::Failed)),
                "{name} playback failed"
            );
            if ready(event) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(12),
                "{name} playback timed out"
            );
            thread::sleep(Duration::from_millis(30));
        }
    }

    async fn fixture_audio(
        State((data, mime)): State<(Vec<u8>, &'static str)>,
        method: Method,
        headers: HeaderMap,
    ) -> Response {
        let range = headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("bytes="))
            .and_then(|value| value.split_once('-'))
            .and_then(|(start, end)| {
                Some((
                    start.parse::<usize>().ok()?,
                    end.parse::<usize>().unwrap_or(data.len() - 1),
                ))
            });
        let mut builder = Response::builder()
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_TYPE, mime);
        let payload = if let Some((start, end)) = range {
            let end = end.min(data.len() - 1);
            if start > end {
                return builder
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{}", data.len()))
                    .body(Body::empty())
                    .unwrap();
            }
            builder = builder.status(StatusCode::PARTIAL_CONTENT).header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{}", data.len()),
            );
            &data[start..=end]
        } else {
            &data
        };
        builder
            .header(header::CONTENT_LENGTH, payload.len())
            .body(if method == Method::HEAD {
                Body::empty()
            } else {
                Body::from(payload.to_vec())
            })
            .unwrap()
    }
}
