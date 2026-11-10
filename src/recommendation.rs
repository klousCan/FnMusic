use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::library::Track;

pub const DAILY_SIZE: usize = 30;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Day {
    pub year: u16,
    pub month: u16,
    pub day: u16,
}

impl Day {
    pub fn label(self) -> String {
        format!("{}年{}月{}日", self.year, self.month, self.day)
    }

    fn ordinal(self) -> u32 {
        let previous = u32::from(self.year).saturating_sub(1);
        let mut days = previous * 365 + previous / 4 - previous / 100 + previous / 400;
        for month in 1..self.month.min(13) {
            days += match month {
                4 | 6 | 9 | 11 => 30,
                2 => {
                    28 + u32::from(
                        self.year % 4 == 0 && (self.year % 100 != 0 || self.year % 400 == 0),
                    )
                }
                _ => 31,
            };
        }
        days + u32::from(self.day)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Mix {
    pub day: Day,
    pub tracks: Vec<Track>,
    pub personalized: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Cache {
    pub current: Mix,
    #[serde(default)]
    recent: Vec<Recent>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Recent {
    day: Day,
    ids: Vec<String>,
}

fn hash(seed: &str, day: Day, id: &str) -> u64 {
    let digest = Sha256::digest(format!(
        "{seed}\0{}-{}-{}\0{id}",
        day.year, day.month, day.day
    ));
    u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 has 32 bytes"))
}

pub fn seed(account: &str, day: Day) -> u64 {
    hash(account, day, "daily-mix-v1")
}

/// Spread a bounded number of requests across the library, including old music.
/// No random-sort endpoint is assumed to exist on the NAS.
pub fn candidate_pages(total: usize, page_size: usize, seed: u64) -> Vec<u32> {
    let count = total
        .div_ceil(page_size.max(1))
        .max(1)
        .min(u32::MAX as usize);
    if count <= 8 {
        return (1..=count as u32).collect();
    }
    let mut pages = vec![1];
    for index in 0..7 {
        let start = 1 + index * (count - 1) / 7;
        let end = 1 + (index + 1) * (count - 1) / 7;
        let offset = seed.rotate_left(index as u32 * 7) as usize % (end - start).max(1);
        pages.push((start + offset + 1) as u32);
    }
    pages.sort_unstable();
    pages.dedup();
    pages
}

impl Cache {
    pub fn ready(&self, day: Day) -> bool {
        self.current.day == day && !self.current.tracks.is_empty()
    }

    pub fn generate(mut self, day: Day, account: &str, tracks: Vec<Track>) -> Self {
        if self.ready(day) {
            return self;
        }
        if self.current.day != day && !self.current.tracks.is_empty() {
            self.recent.insert(
                0,
                Recent {
                    day: self.current.day,
                    ids: self
                        .current
                        .tracks
                        .iter()
                        .map(|track| track.id.clone())
                        .collect(),
                },
            );
            self.recent.truncate(7);
        }
        self.recent.retain(|recent| {
            recent.day.ordinal() < day.ordinal() && day.ordinal() - recent.day.ordinal() <= 7
        });
        let mut unique: BTreeMap<String, Track> = BTreeMap::new();
        for track in tracks.into_iter().filter(|track| !track.id.is_empty()) {
            unique
                .entry(track.id.clone())
                .and_modify(|old| {
                    old.favorite |= track.favorite;
                    if old.cover.is_empty() {
                        old.cover.clone_from(&track.cover);
                    }
                })
                .or_insert(track);
        }
        let favorite_artists: HashSet<_> = unique
            .values()
            .filter(|track| track.favorite && track.artist != "未知艺人")
            .map(|track| track.artist.clone())
            .collect();
        let personalized = !favorite_artists.is_empty();
        let mut ranked: Vec<_> = unique
            .into_values()
            .map(|track| {
                let mut weight = if track.favorite {
                    2.3
                } else if favorite_artists.contains(&track.artist) {
                    1.8
                } else {
                    1.0
                };
                if let Some(recent) = self
                    .recent
                    .iter()
                    .find(|recent| recent.ids.contains(&track.id))
                {
                    weight *= if day.ordinal() - recent.day.ordinal() == 1 {
                        0.12
                    } else {
                        0.4
                    };
                }
                let uniform =
                    (hash(account, day, &track.id) as f64 + 1.0) / (u64::MAX as f64 + 2.0);
                (-uniform.ln() / weight, track)
            })
            .collect();
        ranked.sort_by(|(a, ta), (b, tb)| a.total_cmp(b).then_with(|| ta.id.cmp(&tb.id)));
        let mut selected = HashSet::new();
        let mut artist_counts = BTreeMap::<String, usize>::new();
        let mut album_counts = BTreeMap::<(String, String), usize>::new();
        let mut out = Vec::new();
        // Prefer at most two songs per artist/album, relaxing only for small or
        // homogeneous libraries so the feature still works with a single artist.
        for cap in 2..=DAILY_SIZE {
            for (_, track) in &ranked {
                if out.len() == DAILY_SIZE {
                    break;
                }
                let album = (track.artist.clone(), track.album.clone());
                if selected.contains(&track.id)
                    || artist_counts
                        .get(&track.artist)
                        .copied()
                        .unwrap_or_default()
                        >= cap
                    || (!track.album.is_empty()
                        && album_counts.get(&album).copied().unwrap_or_default() >= cap)
                {
                    continue;
                }
                selected.insert(track.id.clone());
                *artist_counts.entry(track.artist.clone()).or_default() += 1;
                *album_counts.entry(album).or_default() += 1;
                out.push(track.clone());
            }
            if out.len() == DAILY_SIZE || out.len() == ranked.len() {
                break;
            }
        }
        self.current = Mix {
            day,
            tracks: out,
            personalized,
        };
        self
    }
}
