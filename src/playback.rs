use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use crate::library::Track;

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub struct Queue {
    tracks: Vec<Track>,
    current: Option<usize>,
    order: Vec<usize>,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub position_ms: u64,
    pub volume: f32,
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            current: None,
            order: Vec::new(),
            shuffle: false,
            repeat: Repeat::Off,
            position_ms: 0,
            volume: 0.8,
        }
    }
}

impl Queue {
    pub fn restore(&mut self) {
        let mut sorted = self.order.clone();
        sorted.sort_unstable();
        if sorted.into_iter().ne(0..self.tracks.len()) {
            self.order = (0..self.tracks.len()).collect();
        }
        if self.current().is_none() {
            self.current = self.order.first().copied();
            self.position_ms = 0;
        }
        if let Some(track) = self.current().filter(|track| track.duration_ms > 0) {
            self.position_ms = self.position_ms.min(track.duration_ms);
        }
        self.volume = self.volume.clamp(0.0, 1.0);
    }

    pub fn replace(&mut self, tracks: Vec<Track>, start: usize) {
        self.tracks = tracks;
        self.order = (0..self.tracks.len()).collect();
        self.select(start);
        if self.shuffle {
            self.shuffle_from_current();
        }
    }

    pub fn current(&self) -> Option<&Track> {
        self.tracks.get(self.current?)
    }

    pub fn entries(&self) -> impl Iterator<Item = (usize, &Track, bool)> {
        self.order
            .iter()
            .map(|&index| (index, &self.tracks[index], self.current == Some(index)))
    }

    pub fn select(&mut self, index: usize) {
        self.current = self.tracks.get(index).map(|_| index);
        self.position_ms = 0;
    }

    pub fn set_favorite(&mut self, id: &str, on: bool) {
        for track in self.tracks.iter_mut().filter(|track| track.id == id) {
            track.favorite = on;
        }
    }

    fn cursor(&self) -> Option<usize> {
        let current = self.current?;
        self.order.iter().position(|&index| index == current)
    }

    fn shuffle_from_current(&mut self) {
        if let Some(cursor) = self.cursor() {
            self.order.swap(0, cursor);
            self.order[1..].shuffle(&mut rand::rng());
        }
    }

    pub fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        self.order = (0..self.tracks.len()).collect();
        if self.shuffle {
            self.shuffle_from_current();
        }
    }

    pub fn add(&mut self, track: Track, next: bool) {
        let index = self.tracks.len();
        self.tracks.push(track);
        if next && let Some(cursor) = self.cursor() {
            self.order.insert(cursor + 1, index);
        } else {
            self.order.push(index);
        }
        if self.current.is_none() {
            self.select(index);
        }
    }

    pub fn remove(&mut self, index: usize) -> bool {
        let Some(cursor) = self.order.iter().position(|&item| item == index) else {
            return false;
        };
        self.order.remove(cursor);
        self.tracks.remove(index);
        for item in &mut self.order {
            if *item > index {
                *item -= 1;
            }
        }
        if self.current == Some(index) {
            self.current = self
                .order
                .get(cursor)
                .or_else(|| self.order.last())
                .copied();
            self.position_ms = 0;
            true
        } else {
            self.current = self.current.map(|current| {
                if current > index {
                    current - 1
                } else {
                    current
                }
            });
            false
        }
    }

    pub fn move_entry(&mut self, index: usize, delta: isize) {
        if let Some(position) = self.order.iter().position(|&item| item == index)
            && let Some(target) = position.checked_add_signed(delta)
            && target < self.order.len()
        {
            self.order.swap(position, target);
        }
    }

    pub fn previous(&mut self) -> bool {
        let Some(cursor) = self.cursor() else {
            return false;
        };
        if self.position_ms < 3_000 {
            if let Some(previous) = cursor.checked_sub(1) {
                self.select(self.order[previous]);
                return true;
            }
            if self.repeat == Repeat::All
                && let Some(&last) = self.order.last()
            {
                self.select(last);
                return true;
            }
        }
        self.position_ms = 0;
        false
    }

    pub fn next(&mut self, natural_end: bool) -> bool {
        let Some(cursor) = self.cursor() else {
            return false;
        };
        if natural_end && self.repeat == Repeat::One {
            self.position_ms = 0;
            return true;
        }
        let next = self.order.get(cursor + 1).copied().or_else(|| {
            if self.repeat != Repeat::All {
                return None;
            }
            if self.shuffle && self.order.len() > 1 {
                self.order.shuffle(&mut rand::rng());
                if self.current == self.order.first().copied() {
                    self.order.swap(0, 1);
                }
            }
            self.order.first().copied()
        });
        if let Some(next) = next {
            self.select(next);
            true
        } else {
            false
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct LyricLine {
    pub position_ms: Option<u64>,
    pub text: String,
}

pub fn parse_lrc(content: &str) -> Vec<LyricLine> {
    let mut timed = Vec::new();
    let mut plain = Vec::new();
    let mut offset = 0i64;
    for line in content.trim_start_matches('\u{feff}').lines() {
        let mut text = line.trim();
        let mut stamps = Vec::new();
        while let Some(rest) = text.strip_prefix('[').and_then(|s| s.split_once(']')) {
            let (tag, tail) = rest;
            if let Some((key, value)) = tag.split_once(':') {
                if key.eq_ignore_ascii_case("offset") {
                    offset = value.trim().parse().unwrap_or(offset);
                } else if let Some(ms) = parse_time(key, value) {
                    stamps.push(ms);
                }
            } else {
                break;
            }
            text = tail.trim_start();
        }
        if stamps.is_empty() && !text.is_empty() {
            plain.push(LyricLine {
                position_ms: None,
                text: text.into(),
            });
        }
        for stamp in stamps {
            timed.push((stamp, text.to_owned()));
        }
    }
    if timed.is_empty() {
        return plain;
    }
    timed.sort_by_key(|(ms, _)| *ms);
    let mut lines: Vec<LyricLine> = Vec::new();
    for (ms, text) in timed {
        let position_ms = Some(ms.saturating_add_signed(offset));
        if let Some(last) = lines
            .last_mut()
            .filter(|line| line.position_ms == position_ms)
        {
            if !text.is_empty() && last.text != text {
                if !last.text.is_empty() {
                    last.text.push('\n');
                }
                last.text.push_str(&text);
            }
        } else {
            lines.push(LyricLine { position_ms, text });
        }
    }
    lines
}

fn parse_time(minutes: &str, rest: &str) -> Option<u64> {
    let minutes = minutes.parse::<u64>().ok()?;
    let (seconds, fraction) = rest.split_once(['.', ':']).unwrap_or((rest, ""));
    let seconds = seconds.parse::<u64>().ok()?;
    if seconds >= 60 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let milliseconds = fraction
        .bytes()
        .chain(std::iter::repeat(b'0'))
        .take(3)
        .fold(0u64, |value, digit| value * 10 + u64::from(digit - b'0'));
    minutes
        .checked_mul(60_000)?
        .checked_add(seconds * 1000 + milliseconds)
}

pub fn active_lyric(lines: &[LyricLine], ms: u64) -> Option<usize> {
    lines
        .partition_point(|line| line.position_ms.is_some_and(|position| position <= ms))
        .checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_offsets_repeated_tags_and_translations() {
        let lines =
            parse_lrc("[ar:Demo]\r\n[00:04.1][00:01.20]歌词\n[00:01.200]翻译\n[offset:-100]");
        assert_eq!(
            lines[0],
            LyricLine {
                position_ms: Some(1100),
                text: "歌词\n翻译".into()
            }
        );
        assert_eq!(lines[1].position_ms, Some(4000));
        assert_eq!(active_lyric(&lines, 1099), None);
        assert_eq!(active_lyric(&lines, 4000), Some(1));
        assert_eq!(parse_lrc("[ti:Song]\nPlain text")[0].position_ms, None);
    }

    #[test]
    fn queue_survives_reorder_removal_and_restore() {
        let tracks = ["a", "b", "c"]
            .into_iter()
            .map(|id| Track {
                id: id.into(),
                duration_ms: 1000,
                ..Track::default()
            })
            .collect();
        let mut queue = Queue::default();
        queue.replace(tracks, 1);
        queue.move_entry(2, -1);
        assert!(queue.remove(1));
        assert_eq!(queue.current().unwrap().id, "c");
        queue.position_ms = 2500;
        let mut queue: Queue =
            serde_json::from_str(&serde_json::to_string(&queue).unwrap()).unwrap();
        queue.restore();
        assert_eq!(queue.position_ms, 1000);
        queue.repeat = Repeat::All;
        assert!(queue.next(true));
        assert_eq!(queue.current().unwrap().id, "a");
        queue.repeat = Repeat::One;
        assert!(queue.next(true));
        assert_eq!(queue.current().unwrap().id, "a");
    }
}
