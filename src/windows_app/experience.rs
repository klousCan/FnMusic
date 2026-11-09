use std::time::{Duration, Instant};

#[derive(Default)]
pub enum SleepTimer {
    #[default]
    Off,
    Deadline(Instant, u64),
    EndOfTrack,
}

impl SleepTimer {
    pub fn after(minutes: u64) -> Self {
        Self::Deadline(Instant::now() + Duration::from_secs(minutes * 60), minutes)
    }

    pub fn preset(&self) -> Option<u64> {
        match self {
            Self::Deadline(_, minutes) if *minutes > 0 => Some(*minutes),
            _ => None,
        }
    }

    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        match self {
            Self::Deadline(deadline, _) => Some(deadline.saturating_duration_since(now)),
            _ => None,
        }
    }

    pub fn poll(&mut self, now: Instant) -> bool {
        if matches!(self, Self::Deadline(deadline, _) if now >= *deadline) {
            *self = Self::Off;
            true
        } else {
            false
        }
    }

    pub fn track_ended(&mut self) -> bool {
        if matches!(self, Self::EndOfTrack) {
            *self = Self::Off;
            true
        } else {
            false
        }
    }
}

#[derive(Default)]
pub struct LyricFollow {
    browsing: bool,
}

impl LyricFollow {
    pub fn pause(&mut self) {
        self.browsing = true;
    }
    pub fn resume(&mut self) {
        self.browsing = false;
    }
    pub fn following(&self) -> bool {
        !self.browsing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_deadlines_fire_once_and_end_of_track_does_not_fire_early() {
        let now = Instant::now();
        let mut timer = SleepTimer::Deadline(now + Duration::from_secs(60), 1);
        assert!(!timer.poll(now + Duration::from_secs(59)));
        assert_eq!(timer.remaining(now).unwrap().as_secs(), 60);
        assert!(timer.poll(now + Duration::from_secs(60)));
        assert!(!timer.poll(now + Duration::from_secs(61)));
        assert!(!timer.track_ended());
        timer = SleepTimer::EndOfTrack;
        assert!(!timer.poll(now + Duration::from_secs(86400)));
        assert!(timer.track_ended());
        assert!(!timer.track_ended());
    }
}
