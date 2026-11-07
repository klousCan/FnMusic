use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, MutexGuard},
};

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use tokio::{net::TcpListener, runtime::Runtime};
use tokio_util::sync::CancellationToken;

use crate::{api::Session, storage::random_id};

#[derive(Clone)]
struct Lease {
    key: String,
    track_id: String,
    quality: Option<String>,
    session: Arc<Session>,
    stop: CancellationToken,
}

type Active = Arc<Mutex<Option<Lease>>>;

pub struct Relay {
    address: SocketAddr,
    active: Active,
    task: tokio::task::JoinHandle<()>,
}

impl Relay {
    pub fn start(runtime: &Runtime) -> std::io::Result<Self> {
        let socket = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = socket.local_addr()?;
        socket.set_nonblocking(true)?;
        let _guard = runtime.enter();
        let listener = TcpListener::from_std(socket)?;
        let active = Active::default();
        let app = Router::new()
            .route("/audio/{key}/{filename}", get(serve))
            .with_state(active.clone());
        let task = runtime.spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            address,
            active,
            task,
        })
    }

    pub fn issue(
        &self,
        session: Arc<Session>,
        track_id: String,
        format: &str,
        quality: Option<&str>,
    ) -> String {
        let key = random_id();
        let suffix = match format {
            "MP3" => "mp3",
            "FLAC" => "flac",
            "AAC" | "M4A" | "ALAC" => "m4a",
            "PCM" | "WAV" => "wav",
            "OGG" | "VORBIS" => "ogg",
            _ => "audio",
        };
        let lease = Lease {
            key: key.clone(),
            track_id,
            quality: quality.map(str::to_owned),
            session,
            stop: CancellationToken::new(),
        };
        if let Some(previous) = lock(&self.active).replace(lease) {
            previous.stop.cancel();
        }
        format!("http://{}/audio/{key}/track.{suffix}", self.address)
    }

    pub fn revoke(&self) {
        if let Some(lease) = lock(&self.active).take() {
            lease.stop.cancel();
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.revoke();
        self.task.abort();
    }
}

fn lock(active: &Active) -> MutexGuard<'_, Option<Lease>> {
    active.lock().unwrap_or_else(|err| err.into_inner())
}

async fn serve(
    State(active): State<Active>,
    Path((key, _)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let lease = lock(&active)
        .as_ref()
        .filter(|lease| lease.key == key)
        .cloned();
    let Some(lease) = lease else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let range = headers.get(header::RANGE).cloned();
    let fetch = async {
        let response = lease
            .session
            .stream(
                method.clone(),
                &lease.track_id,
                lease.quality.as_deref(),
                range.clone(),
            )
            .await?;
        if method == Method::HEAD && response.status() == StatusCode::METHOD_NOT_ALLOWED {
            return lease
                .session
                .stream(
                    Method::GET,
                    &lease.track_id,
                    lease.quality.as_deref(),
                    range,
                )
                .await;
        }
        Ok(response)
    };
    let upstream = tokio::select! {
        biased;
        _ = lease.stop.cancelled() => return StatusCode::NOT_FOUND.into_response(),
        result = fetch => match result {
            Ok(response) => response,
            Err(crate::api::Error::Expired) => return StatusCode::UNAUTHORIZED.into_response(),
            Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
        }
    };
    let status = upstream.status();
    if !matches!(
        status,
        StatusCode::OK | StatusCode::PARTIAL_CONTENT | StatusCode::RANGE_NOT_SATISFIABLE
    ) {
        return if status.is_client_error() || status.is_server_error() {
            status.into_response()
        } else {
            StatusCode::BAD_GATEWAY.into_response()
        };
    }

    let mut response = Response::builder()
        .status(status)
        .header(header::CACHE_CONTROL, "no-store");
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_LENGTH,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
    ] {
        if let Some(value) = upstream.headers().get(&name) {
            response = response.header(name, value.clone());
        }
    }
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from_stream(futures_util::stream::unfold(
            (upstream, lease.stop),
            |(mut response, stop)| async {
                let chunk = tokio::select! {
                    biased;
                    _ = stop.cancelled() => return None,
                    chunk = response.chunk() => chunk,
                };
                match chunk {
                    Ok(Some(bytes)) => Some((Ok(bytes), (response, stop))),
                    Ok(None) => None,
                    Err(err) => Some((Err(err), (response, stop))),
                }
            },
        ))
    };
    response
        .body(body)
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}
