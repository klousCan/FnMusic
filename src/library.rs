use reqwest::Method;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::api::{Error, Result, Session};

const PAGE_SIZE: &str = "100";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub cover: String,
    pub duration_ms: u64,
    pub format: String,
    #[serde(default)]
    pub sample_rate: u64,
    #[serde(default)]
    pub bit_depth: u64,
    #[serde(default)]
    pub bitrate: u64,
    pub favorite: bool,
}

impl Track {
    pub fn quality_label(&self) -> Option<&'static str> {
        let lossless = matches!(
            self.format.as_str(),
            "FLAC" | "ALAC" | "APE" | "WAVPACK" | "PCM"
        ) || self.format.starts_with("PCM_");
        if !lossless {
            return None;
        }
        Some(if self.bit_depth >= 24 && self.sample_rate > 48_000 {
            "Hi-Res 无损"
        } else {
            "无损"
        })
    }

    pub fn audio_description(&self) -> String {
        let mut parts = Vec::new();
        if !self.format.is_empty() {
            parts.push(self.format.clone());
        }
        if self.bit_depth > 0 {
            parts.push(format!("{} bit", self.bit_depth));
        }
        if self.sample_rate > 0 {
            parts.push(format!("{} kHz", self.sample_rate as f64 / 1000.0));
        }
        if self.bitrate > 0 {
            parts.push(format!("{} kbps", self.bitrate / 1000));
        }
        parts.join(" · ")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Album,
    Artist,
    Playlist,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collection {
    pub kind: Kind,
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub cover: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Daily,
    Tracks,
    Albums,
    Artists,
    Playlists,
    Favorites,
    Detail(Collection),
    Search(String),
}

impl Page {
    pub fn title(&self) -> &str {
        match self {
            Self::Home => "首页",
            Self::Daily => "每日推荐",
            Self::Tracks => "歌曲",
            Self::Albums => "专辑",
            Self::Artists => "艺人",
            Self::Playlists => "歌单",
            Self::Favorites => "喜欢的歌曲",
            Self::Detail(collection) => &collection.title,
            Self::Search(_) => "搜索结果",
        }
    }
}

#[derive(Default)]
pub struct Listing {
    pub albums: Vec<Collection>,
    pub artists: Vec<Collection>,
    pub playlists: Vec<Collection>,
    pub tracks: Vec<Track>,
    pub has_more: bool,
}

impl Listing {
    pub fn append(&mut self, more: Self) {
        self.albums.extend(more.albums);
        self.artists.extend(more.artists);
        self.playlists.extend(more.playlists);
        self.tracks.extend(more.tracks);
        self.has_more = more.has_more;
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawEntity {
    #[serde(deserialize_with = "text")]
    guid: String,
    #[serde(deserialize_with = "text")]
    name: String,
    #[serde(deserialize_with = "text")]
    cover_id: String,
    artists: Option<Vec<RawEntity>>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawSpec {
    #[serde(deserialize_with = "text")]
    codec: String,
    #[serde(deserialize_with = "text")]
    path: String,
    #[serde(deserialize_with = "number")]
    sample_rate: u64,
    #[serde(deserialize_with = "number")]
    bit_depth: u64,
    #[serde(deserialize_with = "number")]
    bitrate: u64,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawTrack {
    #[serde(deserialize_with = "text")]
    guid: String,
    #[serde(deserialize_with = "text")]
    title: String,
    #[serde(deserialize_with = "text")]
    cover_id: String,
    #[serde(deserialize_with = "number")]
    duration: u64,
    is_favorite: Option<bool>,
    album: Option<RawEntity>,
    artists: Option<Vec<RawEntity>>,
    audio_spec: Option<RawSpec>,
}

fn text<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(value) => value,
        serde_json::Value::Number(value) => value.to_string(),
        _ => String::new(),
    })
}

fn number<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<u64, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(value) => value.parse().unwrap_or_default(),
        serde_json::Value::Number(value) => value.as_u64().unwrap_or_default(),
        _ => 0,
    })
}

fn cover_id(raw: &str) -> &str {
    if raw.len() == 32 && raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return raw;
    }
    raw.split_once('_')
        .filter(|(kind, id)| {
            matches!(*kind, "album" | "artist" | "track")
                && !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        .map_or("", |_| raw)
}

fn artist_names(artists: Option<Vec<RawEntity>>) -> String {
    artists
        .unwrap_or_default()
        .into_iter()
        .map(|artist| artist.name)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

fn fallback(value: String, alternative: &str) -> String {
    if value.is_empty() {
        alternative.to_owned()
    } else {
        value
    }
}

impl RawEntity {
    fn map(self, kind: Kind) -> Option<Collection> {
        if self.guid.is_empty() {
            return None;
        }
        let default_title = match kind {
            Kind::Album => "未知专辑",
            Kind::Artist => "未知艺人",
            Kind::Playlist => "未命名歌单",
        };
        Some(Collection {
            kind,
            id: self.guid,
            title: fallback(self.name, default_title),
            subtitle: artist_names(self.artists),
            cover: cover_id(&self.cover_id).into(),
        })
    }
}

impl RawTrack {
    fn map(self, known_cover: &str) -> Option<Track> {
        if self.guid.is_empty() {
            return None;
        }
        let album = self.album.unwrap_or_default();
        let cover = [
            cover_id(&self.cover_id),
            cover_id(&album.cover_id),
            known_cover,
        ]
        .into_iter()
        .find(|id| !id.is_empty())
        .unwrap_or_default()
        .to_owned();
        let spec = self.audio_spec.unwrap_or_default();
        let format = if spec.codec.is_empty() {
            spec.path
                .rsplit_once('.')
                .map_or("", |(_, ext)| ext)
                .to_uppercase()
        } else {
            spec.codec.to_uppercase()
        };
        Some(Track {
            id: self.guid,
            title: fallback(self.title, "未知歌曲"),
            artist: fallback(artist_names(self.artists), "未知艺人"),
            album: album.name,
            cover,
            duration_ms: self.duration,
            format,
            sample_rate: spec.sample_rate,
            bit_depth: spec.bit_depth,
            bitrate: spec.bitrate,
            favorite: self.is_favorite.unwrap_or(false),
        })
    }
}

#[derive(Deserialize)]
struct RawPage<T> {
    list: Option<Vec<T>>,
    total: Option<usize>,
}

async fn rows<T: DeserializeOwned>(
    session: &Session,
    path: &str,
    params: &[(&str, &str)],
    page: u32,
    size: &str,
) -> Result<(Vec<T>, bool)> {
    let page_string = page.to_string();
    let mut query = params.to_vec();
    query.extend([("page", page_string.as_str()), ("size", size)]);
    let response: RawPage<T> = session
        .json(Method::GET, path, &query, None)
        .await?
        .ok_or(Error::Format)?;
    let list = response.list.unwrap_or_default();
    let size = size.parse::<usize>().unwrap_or_default();
    let more = !list.is_empty()
        && response.total.map_or(list.len() >= size, |total| {
            (page.saturating_sub(1) as usize)
                .saturating_mul(size)
                .saturating_add(list.len())
                < total
        });
    Ok((list, more))
}

async fn collections(
    session: &Session,
    kind: Kind,
    path: &str,
    query: &[(&str, &str)],
    page: u32,
    size: &str,
) -> Result<Listing> {
    let (list, has_more) = rows::<RawEntity>(session, path, query, page, size).await?;
    let items = list.into_iter().filter_map(|item| item.map(kind)).collect();
    let mut result = Listing {
        has_more,
        ..Listing::default()
    };
    match kind {
        Kind::Album => result.albums = items,
        Kind::Artist => result.artists = items,
        Kind::Playlist => result.playlists = items,
    }
    Ok(result)
}

async fn tracks(
    session: &Session,
    path: &str,
    query: &[(&str, &str)],
    page: u32,
    size: &str,
    cover: &str,
) -> Result<Listing> {
    let (list, has_more) = rows::<RawTrack>(session, path, query, page, size).await?;
    let mut tracks = Vec::new();
    let mut missing = Vec::new();
    for item in list {
        let album_id = item
            .album
            .as_ref()
            .map(|album| album.guid.clone())
            .unwrap_or_default();
        if let Some(track) = item.map(cover) {
            if track.cover.is_empty() && !album_id.is_empty() {
                missing.push((tracks.len(), album_id));
            }
            tracks.push(track);
        }
    }
    fill_track_covers(session, &mut tracks, &missing).await;
    Ok(Listing {
        tracks,
        has_more,
        ..Listing::default()
    })
}

async fn fill_track_covers(session: &Session, tracks: &mut [Track], missing: &[(usize, String)]) {
    // Playlist responses can contain a track GUID as coverId and omit the
    // album cover. Resolve only missing artwork, once per album in this page.
    if !missing.is_empty() {
        use futures_util::{StreamExt, stream};
        let ids: std::collections::HashSet<_> = missing.iter().map(|(_, id)| id.clone()).collect();
        let covers: std::collections::HashMap<_, _> = stream::iter(ids)
            .map(|id| async move {
                let album = session
                    .json::<RawEntity>(
                        Method::GET,
                        "/api/v1/album/detail",
                        &[("albumGUID", id.as_str())],
                        None,
                    )
                    .await;
                let cover = album
                    .ok()
                    .flatten()
                    .map(|album| cover_id(&album.cover_id).to_owned())
                    .unwrap_or_default();
                (id, cover)
            })
            .buffer_unordered(3)
            .collect()
            .await;
        for (index, id) in missing {
            if let Some(cover) = covers.get(id) {
                tracks[*index].cover = cover.clone();
            }
        }
    }
}

pub struct DailyCandidates {
    pub tracks: Vec<Track>,
    albums: std::collections::HashMap<String, String>,
}

pub async fn daily_candidates(session: &Session, seed: u64) -> Result<DailyCandidates> {
    use futures_util::{StreamExt, stream};
    async fn page(session: &Session, path: &str, number: u32) -> Result<RawPage<RawTrack>> {
        session
            .json(
                Method::GET,
                path,
                &[
                    ("page", &number.to_string()),
                    ("size", "500"),
                    ("sort", "createdAt,desc"),
                ],
                None,
            )
            .await?
            .ok_or(Error::Format)
    }
    let first = page(session, "/api/v1/track/list", 1).await?;
    let first_rows = first.list.unwrap_or_default();
    let total = first.total;
    let pages =
        crate::recommendation::candidate_pages(total.unwrap_or(first_rows.len()), 500, seed);
    let mut raw = first_rows;
    let more: Vec<_> = stream::iter(pages.into_iter().filter(|page| *page != 1))
        .map(|number| page(session, "/api/v1/track/list", number))
        .buffered(2)
        .collect()
        .await;
    for result in more {
        raw.extend(result?.list.unwrap_or_default());
    }
    if total.is_none() && raw.len() == 500 {
        // Older NAS responses may omit total; read sequentially until a short
        // page, while retaining the same request bound as sampled libraries.
        for number in 2..=8 {
            let rows = page(session, "/api/v1/track/list", number)
                .await?
                .list
                .unwrap_or_default();
            let more = rows.len() >= 500;
            raw.extend(rows);
            if !more {
                break;
            }
        }
    }
    // Favorites are optional taste signals; permission/network errors must not
    // prevent discovery from the rest of an otherwise accessible library.
    for number in 1..=2 {
        match rows::<RawTrack>(
            session,
            "/api/v1/favorite-track/list",
            &[("sort", "favoriteAt,desc")],
            number,
            "500",
        )
        .await
        {
            Ok((mut favorites, more)) => {
                for track in &mut favorites {
                    track.is_favorite = Some(true);
                }
                raw.extend(favorites);
                if !more {
                    break;
                }
            }
            Err(Error::Expired) => return Err(Error::Expired),
            Err(_) => break,
        }
    }
    let mut result = DailyCandidates {
        tracks: Vec::new(),
        albums: Default::default(),
    };
    for item in raw {
        if let Some(album) = &item.album {
            if !album.guid.is_empty() {
                result.albums.insert(item.guid.clone(), album.guid.clone());
            }
        }
        if let Some(track) = item.map("") {
            result.tracks.push(track);
        }
    }
    Ok(result)
}

pub async fn daily_covers(session: &Session, tracks: &mut [Track], candidates: &DailyCandidates) {
    let missing: Vec<_> = tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| track.cover.is_empty())
        .filter_map(|(index, track)| {
            candidates
                .albums
                .get(&track.id)
                .map(|album| (index, album.clone()))
        })
        .collect();
    fill_track_covers(session, tracks, &missing).await;
}

pub async fn load(session: &Session, page: &Page, number: u32) -> Result<Listing> {
    use Kind::{Album, Artist, Playlist};

    match page {
        Page::Daily => Err(Error::Invalid("请从客户端的每日推荐入口打开".into())),
        Page::Home | Page::Albums => {
            collections(
                session,
                Album,
                "/api/v1/album/list",
                &[("sort", "newTrackAddedAt,desc")],
                number,
                PAGE_SIZE,
            )
            .await
        }
        Page::Tracks => {
            tracks(
                session,
                "/api/v1/track/list",
                &[("sort", "createdAt,desc")],
                number,
                PAGE_SIZE,
                "",
            )
            .await
        }
        Page::Artists => {
            collections(
                session,
                Artist,
                "/api/v1/artist/list",
                &[],
                number,
                PAGE_SIZE,
            )
            .await
        }
        Page::Playlists => {
            collections(
                session,
                Playlist,
                "/api/v1/playlist/list",
                &[],
                number,
                PAGE_SIZE,
            )
            .await
        }
        Page::Favorites => {
            let mut result = tracks(
                session,
                "/api/v1/favorite-track/list",
                &[("sort", "favoriteAt,desc")],
                number,
                PAGE_SIZE,
                "",
            )
            .await?;
            for track in &mut result.tracks {
                track.favorite = true;
            }
            Ok(result)
        }
        Page::Detail(entity) => {
            let id = entity.id.as_str();
            match entity.kind {
                Album => {
                    tracks(
                        session,
                        "/api/v1/track/album-detail/list",
                        &[("albumGUID", id)],
                        number,
                        PAGE_SIZE,
                        &entity.cover,
                    )
                    .await
                }
                Artist => {
                    let params = [("artistGUID", id)];
                    let albums = collections(
                        session,
                        Album,
                        "/api/v1/album/artist-detail/list",
                        &params,
                        number,
                        PAGE_SIZE,
                    );
                    let tracks = tracks(
                        session,
                        "/api/v1/track/artist-detail/list",
                        &params,
                        number,
                        PAGE_SIZE,
                        "",
                    );
                    let (albums, mut tracks) = tokio::try_join!(albums, tracks)?;
                    tracks.has_more |= albums.has_more;
                    tracks.albums = albums.albums;
                    Ok(tracks)
                }
                Playlist => {
                    tracks(
                        session,
                        "/api/v1/track/playlist-detail/list",
                        &[("playlistGUID", id)],
                        number,
                        PAGE_SIZE,
                        "",
                    )
                    .await
                }
            }
        }
        Page::Search(query) => {
            let params = [("q", query.as_str())];
            let (songs, albums, artists) = tokio::try_join!(
                tracks(
                    session,
                    "/api/v1/search/track",
                    &params,
                    number,
                    PAGE_SIZE,
                    ""
                ),
                collections(
                    session,
                    Album,
                    "/api/v1/search/album",
                    &params,
                    number,
                    PAGE_SIZE
                ),
                collections(
                    session,
                    Artist,
                    "/api/v1/search/artist",
                    &params,
                    number,
                    PAGE_SIZE
                ),
            )?;
            Ok(Listing {
                has_more: songs.has_more || albums.has_more || artists.has_more,
                tracks: songs.tracks,
                albums: albums.albums,
                artists: artists.artists,
                ..Listing::default()
            })
        }
    }
}

pub async fn add_playlist_tracks(session: &Session, id: &str, tracks: &[String]) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({ "guid": id, "trackGUIDs": tracks }))
        .map_err(|_| Error::Format)?;
    session
        .json::<serde_json::Value>(Method::POST, "/api/v1/playlist/add-track", &[], Some(body))
        .await?;
    Ok(())
}

pub async fn favorite(session: &Session, id: &str, on: bool) -> Result<()> {
    let path = if on {
        "/api/v1/favorite-track/create"
    } else {
        "/api/v1/favorite-track/delete"
    };
    let body =
        serde_json::to_vec(&serde_json::json!({ "trackGUID": id })).map_err(|_| Error::Format)?;
    session
        .json::<serde_json::Value>(Method::POST, path, &[], Some(body))
        .await?;
    Ok(())
}

pub async fn lyric(session: &Session, id: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct Lyric {
        content: Option<String>,
    }

    let data: RawPage<Lyric> = session
        .json(
            Method::GET,
            "/api/v1/lyric/list",
            &[("trackGUID", id)],
            None,
        )
        .await?
        .ok_or(Error::Format)?;
    Ok(data
        .list
        .unwrap_or_default()
        .into_iter()
        .filter_map(|lyric| lyric.content)
        .filter(|content| !content.trim().is_empty())
        .max_by_key(|content| {
            crate::playback::parse_lrc(content)
                .iter()
                .filter(|line| line.position_ms.is_some())
                .count()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_tracks_restore_and_quality_uses_codec_and_source_resolution() {
        let old: Track = serde_json::from_str(
            r#"{
            "id":"old","title":"Old","artist":"","album":"","cover":"",
            "duration_ms":1000,"format":"FLAC","favorite":false
        }"#,
        )
        .unwrap();
        assert_eq!(old.sample_rate, 0);
        assert_eq!(old.quality_label(), Some("无损"));
        let mut track = Track {
            format: "FLAC".into(),
            sample_rate: 96_000,
            bit_depth: 24,
            ..Default::default()
        };
        assert_eq!(track.quality_label(), Some("Hi-Res 无损"));
        assert!(track.audio_description().contains("96 kHz"));
        track.sample_rate = 48_000;
        assert_eq!(track.quality_label(), Some("无损"));
        track.format = "MP3".into();
        assert_eq!(track.quality_label(), None);
    }

    #[test]
    fn maps_numeric_guids_and_duration_and_skips_invalid_covers() {
        assert_eq!(
            cover_id("e03fb3795678482bae1ddd87bb0d40aa"),
            "e03fb3795678482bae1ddd87bb0d40aa"
        );
        assert_eq!(cover_id("https://example.com/cover"), "");
        let raw = serde_json::from_str::<RawTrack>(
            r#"{
            "guid":42,"title":"晴天","duration":"185000","coverId":"012345",
            "album":{"name":"叶惠美","coverId":"album_ab12"},
            "artists":[{"name":"周杰伦"},{"name":null}],
            "audioSpec":{"path":"song.flac","sampleRate":"96000","bitDepth":24,"bitrate":"920000"}
        }"#,
        )
        .unwrap();
        let track = raw.map("").unwrap();
        assert_eq!(track.id, "42");
        assert_eq!(track.duration_ms, 185000);
        assert_eq!(track.artist, "周杰伦");
        assert_eq!(track.cover, "album_ab12");
        assert_eq!(track.format, "FLAC");
        assert_eq!(track.sample_rate, 96_000);
        assert_eq!(track.bit_depth, 24);
        assert_eq!(track.bitrate, 920_000);
    }
}
