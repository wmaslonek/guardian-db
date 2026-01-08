use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// A [Lamport clock] for partial chronological ordering of unconnected events.
///
/// [Lamport clock]: https://en.wikipedia.org/wiki/Lamport_clock
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct LamportClock {
    id: String,
    time: u64,
}

impl LamportClock {
    /// Constructs a new Lamport clock with the given identifier.
    pub fn new(id: &str) -> LamportClock {
        LamportClock {
            id: id.to_owned(),
            time: 0,
        }
    }

    /// Sets the time of the (newly constructed) Lamport clock.
    ///
    /// ```ignore
    /// let clock = LamportClock::new("some_id").set_time(128);
    /// ```
    pub fn set_time(mut self, time: u64) -> LamportClock {
        self.time = time;
        self
    }

    /// Returns the current time of the Lamport clock.
    pub fn time(&self) -> u64 {
        self.time
    }

    /// Returns the identifier of the Lamport clock.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Advances the time of the Lamport clock.
    pub fn tick(&mut self) {
        self.time += 1;
    }

    /// Merges `o` to `self` in the following manner:
    /// * if `self.time < o.time`, set `self.time = o.time`,
    ///   otherwise do nothing
    /// * `o` is never modified
    pub fn merge(&mut self, o: &LamportClock) {
        if self.time < o.time {
            self.time = o.time;
        }
    }
}

impl PartialEq for LamportClock {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time && self.id == other.id
    }
}

impl Eq for LamportClock {}

impl Ord for LamportClock {
    fn cmp(&self, other: &Self) -> Ordering {
        let delta = self.time as i64 - other.time as i64;
        if delta == 0 && self.id != other.id {
            if self.id < other.id {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        } else if delta < 0 {
            Ordering::Less
        } else if delta > 0 {
            Ordering::Greater
        }
        //is this necessary/hoped for?
        else {
            Ordering::Equal
        }
    }
}

impl PartialOrd for LamportClock {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
