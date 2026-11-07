use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use fnmusic::{
    api::{
        self, Api, AuthFuture, Authorization, Credentials, Session, SessionHooks, normalize_base,
    },
    library::{self, Page},
    playback::parse_lrc,
    relay::Relay,
};
use serde_json::json;
use tokio::{net::TcpListener, runtime::Runtime};

#[derive(Default)]
struct Fixture {
    exchanges: AtomicUsize,
    authorizations: AtomicUsize,
    saved: AtomicUsize,
}

impl SessionHooks for ArcFixture {
    fn authorize(&self, request: Authorization, _: String, silent: bool) -> AuthFuture {
        assert!(silent);
        assert_eq!(request.sign_in.path(), "/signin");
        self.0.authorizations.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok("new-code".into()) })
    }

    fn save(&self, _: &Credentials) -> api::Result<()> {
        self.0.saved.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct ArcFixture(Arc<Fixture>);

async fn exchange(
    State(state): State<Arc<Fixture>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    state.exchanges.fetch_add(1, Ordering::SeqCst);
    assert_eq!(payload["deviceId"], "test-device");
    let token = match payload["code"].as_str() {
        Some("old-code") => "old-token",
        Some("new-code") => "new-token",
        _ => panic!("unknown code"),
    };
    Json(json!({"code": 0, "data": {"userToken": token, "user": {"username":"tester"}}}))
}

fn valid(headers: &HeaderMap) -> bool {
    assert!(headers.contains_key("authx"));
    headers
        .get(header::COOKIE)
        .is_some_and(|cookie| cookie == "music-token=new-token")
}

async fn page(
    headers: HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if !valid(&headers) {
        return Json(json!({"code":99999})).into_response();
    }
    let page = query.get("page").unwrap().parse::<usize>().unwrap();
    let list: Vec<_> = if page == 1 {
        (0..100)
            .map(|i| json!({"guid":i+1,"title":format!("歌曲 {i}"),"duration":1000}))
            .collect()
    } else {
        vec![json!({"guid":101,"title":"第二页","duration":2000})]
    };
    Json(json!({"code":0,"data":{"list":list,"total":101}})).into_response()
}

async fn stream(method: Method, headers: HeaderMap) -> Response {
    assert!(valid(&headers));
    let builder = Response::builder()
        .header(header::CONTENT_TYPE, "audio/wav")
        .header(header::ACCEPT_RANGES, "bytes");
    let (builder, bytes) = match headers
        .get(header::RANGE)
        .and_then(|range| range.to_str().ok())
    {
        Some("bytes=2-4") => (
            builder
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_RANGE, "bytes 2-4/8"),
            "CDE",
        ),
        Some("bytes=8-") => (
            builder
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, "bytes */8"),
            "",
        ),
        None => (builder, "ABCDEFGH"),
        Some(other) => panic!("unexpected range: {other}"),
    };
    builder
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(if method == Method::HEAD {
            Body::empty()
        } else {
            Body::from(bytes)
        })
        .unwrap()
}

#[test]
fn parallel_renewal_pagination_errors_and_range_relay() {
    let runtime = Runtime::new().unwrap();
    let relay = Relay::start(&runtime).unwrap();
    runtime.block_on(async {
        let fixture = Arc::new(Fixture::default());
        let mock = Router::new()
            .route(
                "/music/api/v1/sys/config",
                get(|| async { Json(json!({"code":0,"data":{"nasOAuth":{"clientId":"MOCK"}}})) }),
            )
            .route("/music/api/v1/user/auth-login", post(exchange))
            .route("/music/api/v1/track/list", get(page))
            .route("/music/api/v1/track/stream", get(stream))
            .route(
                "/music/api/v1/error",
                get(|| async { (StatusCode::BAD_GATEWAY, "not JSON") }),
            )
            .route("/music/api/v1/malformed", get(|| async { "not JSON" }))
            .with_state(fixture.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = normalize_base(&listener.local_addr().unwrap().to_string()).unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, mock).await.unwrap();
        });
        let api = Api::new().unwrap();
        let credentials = api
            .exchange(
                base,
                "test-device".into(),
                "test-profile".into(),
                "old-code".into(),
            )
            .await
            .unwrap();
        let session = Arc::new(Session::new(
            api,
            credentials,
            Box::new(ArcFixture(fixture.clone())),
        ));

        let (first, second) = tokio::join!(
            library::load(&session, &Page::Tracks, 1),
            library::load(&session, &Page::Tracks, 2),
        );
        let mut listing = first.unwrap();
        assert!(listing.has_more);
        listing.append(second.unwrap());
        assert!(!listing.has_more);
        assert_eq!(listing.tracks.len(), 101);
        assert_eq!(listing.tracks[100].title, "第二页");
        assert_eq!(fixture.exchanges.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.authorizations.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.saved.load(Ordering::SeqCst), 1);

        assert!(matches!(
            session
                .json::<serde_json::Value>(Method::GET, "/api/v1/error", &[], None)
                .await,
            Err(api::Error::Http(StatusCode::BAD_GATEWAY))
        ));
        assert!(matches!(
            session
                .json::<serde_json::Value>(Method::GET, "/api/v1/malformed", &[], None)
                .await,
            Err(api::Error::Format)
        ));

        let endpoint = relay.issue(session, "101".into(), "WAV", None);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let head = client.head(&endpoint).send().await.unwrap();
        assert_eq!(head.headers()[header::CONTENT_LENGTH], "8");
        assert!(head.bytes().await.unwrap().is_empty());
        let part = client
            .get(&endpoint)
            .header(header::RANGE, "bytes=2-4")
            .send()
            .await
            .unwrap();
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(part.headers()[header::CONTENT_RANGE], "bytes 2-4/8");
        assert_eq!(part.bytes().await.unwrap().as_ref(), b"CDE");
        let invalid = client
            .get(&endpoint)
            .header(header::RANGE, "bytes=8-")
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(invalid.headers()[header::CONTENT_RANGE], "bytes */8");
        relay.revoke();
        assert_eq!(
            client.get(endpoint).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        server.abort();
    });
}

struct ManualOnly;

impl SessionHooks for ManualOnly {
    fn authorize(&self, _: Authorization, _: String, _: bool) -> AuthFuture {
        Box::pin(async { Err(api::Error::Expired) })
    }
    fn save(&self, _: &Credentials) -> api::Result<()> {
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires the upstream FN-Music-Apple mock server"]
async fn upstream_mock_contract() {
    let mock = std::env::var("FNMUSIC_MOCK_URL").unwrap_or_else(|_| "127.0.0.1:5666".into());
    let base = normalize_base(&mock).unwrap();
    assert!(matches!(base.host_str(), Some("127.0.0.1" | "localhost")));
    let api = Api::new().unwrap();
    let auth = api.authorization(&base).await.unwrap();
    let form = auth.sign_in.join("/signin/submit").unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .post(form)
        .form(&[("r", auth.callback.as_str()), ("u", "test"), ("p", "test")])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    let redirect = response
        .split("location.replace(")
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap();
    let redirect = serde_json::from_str::<String>(redirect).unwrap();
    let code = auth
        .callback_code(&redirect.parse().unwrap())
        .unwrap()
        .unwrap();
    let credentials = api
        .exchange(base, "mock-device".into(), "mock-profile".into(), code)
        .await
        .unwrap();
    let session = Session::new(api, credentials, Box::new(ManualOnly));
    session.validate().await.unwrap();
    assert!(
        !library::load(&session, &Page::Home, 1)
            .await
            .unwrap()
            .albums
            .is_empty()
    );
    assert!(
        !library::load(&session, &Page::Artists, 1)
            .await
            .unwrap()
            .artists
            .is_empty()
    );
    assert!(
        !library::load(&session, &Page::Playlists, 1)
            .await
            .unwrap()
            .playlists
            .is_empty()
    );
    let tracks = library::load(&session, &Page::Tracks, 1)
        .await
        .unwrap()
        .tracks;
    let track = tracks.first().unwrap();
    assert!(!session.cover(&track.cover, "160").await.unwrap().is_empty());
    assert!(
        !library::load(&session, &Page::Search(track.title.clone()), 1)
            .await
            .unwrap()
            .tracks
            .is_empty()
    );
    library::favorite(&session, &track.id, true).await.unwrap();
    assert!(
        library::load(&session, &Page::Favorites, 1)
            .await
            .unwrap()
            .tracks
            .iter()
            .any(|song| song.id == track.id)
    );
    assert!(!parse_lrc(&library::lyric(&session, &track.id).await.unwrap()).is_empty());
    let response = session
        .stream(
            Method::GET,
            &track.id,
            None,
            Some("bytes=0-11".parse().unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(&response.bytes().await.unwrap()[..4], b"RIFF");
    library::favorite(&session, &track.id, false).await.unwrap();
    session.logout().await;
    assert!(session.expired() || matches!(session.validate().await, Err(api::Error::Expired)));
}
