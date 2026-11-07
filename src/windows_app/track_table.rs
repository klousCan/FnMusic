use std::collections::HashSet;

use fnmusic::library::Track;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Title,
    Artist,
    Album,
    Duration,
}

impl Column {
    pub fn title(self) -> &'static str {
        match self {
            Self::Title => "歌曲",
            Self::Artist => "艺人",
            Self::Album => "专辑",
            Self::Duration => "时长",
        }
    }
}

#[derive(Default)]
pub struct Table {
    pub sort: Option<(Column, bool)>,
    pub selected: HashSet<String>,
    cursor: Option<String>,
    anchor: Option<String>,
}

impl Table {
    pub fn toggle_sort(&mut self, column: Column, tracks: &mut [Track]) {
        let descending = self.sort.is_some_and(|sort| sort == (column, false));
        self.sort = Some((column, descending));
        self.sort_tracks(tracks);
    }

    pub fn sort_tracks(&self, tracks: &mut [Track]) {
        let Some((column, descending)) = self.sort else {
            return;
        };
        if column == Column::Duration {
            tracks.sort_by_key(|track| {
                if descending {
                    u64::MAX - track.duration_ms
                } else {
                    track.duration_ms
                }
            });
        } else {
            let key = |track: &Track| {
                match column {
                    Column::Title => &track.title,
                    Column::Artist => &track.artist,
                    Column::Album => &track.album,
                    Column::Duration => unreachable!(),
                }
                .to_lowercase()
            };
            if descending {
                tracks.sort_by_cached_key(|track| std::cmp::Reverse(key(track)));
            } else {
                tracks.sort_by_cached_key(key);
            }
        }
    }

    pub fn cursor(&self, tracks: &[Track]) -> Option<usize> {
        let cursor = self.cursor.as_deref()?;
        tracks.iter().position(|track| track.id == cursor)
    }

    pub fn select(&mut self, tracks: &[Track], index: usize, extend: bool, toggle: bool) {
        let id = &tracks[index].id;
        self.cursor = Some(id.clone());
        if extend
            && let Some(anchor) = self
                .anchor
                .as_deref()
                .and_then(|id| tracks.iter().position(|track| track.id == id))
        {
            if !toggle {
                self.selected.clear();
            }
            self.selected.extend(
                tracks[anchor.min(index)..=anchor.max(index)]
                    .iter()
                    .map(|track| track.id.clone()),
            );
        } else {
            self.anchor = Some(id.clone());
            if toggle {
                if !self.selected.remove(id) {
                    self.selected.insert(id.clone());
                }
            } else {
                self.selected.clear();
                self.selected.insert(id.clone());
            }
        }
    }

    pub fn retain(&mut self, tracks: &[Track]) {
        let ids: HashSet<_> = tracks.iter().map(|track| track.id.as_str()).collect();
        self.selected.retain(|id| ids.contains(id.as_str()));
        if self
            .cursor
            .as_ref()
            .is_some_and(|id| !ids.contains(id.as_str()))
        {
            self.cursor = None;
        }
        if self
            .anchor
            .as_ref()
            .is_some_and(|id| !ids.contains(id.as_str()))
        {
            self.anchor = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_follows_song_ids_when_sorted_and_shift_selects_the_visible_order() {
        let mut tracks: Vec<_> = ["C", "a", "b"]
            .into_iter()
            .map(|name| Track {
                id: name.into(),
                title: name.into(),
                ..Default::default()
            })
            .collect();
        let mut table = Table::default();
        table.select(&tracks, 1, false, false);
        table.toggle_sort(Column::Title, &mut tracks);
        assert_eq!(table.cursor(&tracks), Some(0));
        table.select(&tracks, 2, true, false);
        assert_eq!(table.selected.len(), 3);
        table.select(&tracks, 1, false, true);
        assert!(!table.selected.contains("b"));
        table.toggle_sort(Column::Title, &mut tracks);
        assert_eq!(tracks[0].id, "C");
        table.retain(&tracks[1..]);
        assert_eq!(table.selected, HashSet::from(["a".into()]));
    }
}
