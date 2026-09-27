//! The game era a source is about: Splatoon 2 or Splatoon 3 Salmon Run.
//!
//! Splatoon 3 launched on 2022-09-09; community material from before is
//! about Splatoon 2's Salmon Run, whose bosses, stages and rules differ in
//! parts. Everything gets an era from its date ([`era`]) and may be
//! corrected later from evidence (a video's HUD, a wave-start table:
//! [`crate::corpus::WaveStarts::game`]). Retrieval prefers the current
//! game a little ([`crate::index::score`]) and labels the older era for
//! the model ([`Game::era_label`]).

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// The game a source is about
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Game {
    /// Splatoon 2 (2017-2022)
    S2,
    /// Splatoon 3 (2022-)
    S3,
}

/// The day Splatoon 3 launched
pub const S3_LAUNCH: NaiveDate = match NaiveDate::from_ymd_opt(2022, 9, 9) {
    Some(d) => d,
    None => unreachable!(),
};

/// The era something from `at` is about, by the date alone
pub fn era(at: DateTime<Utc>) -> Game {
    if at.date_naive() < S3_LAUNCH {
        Game::S2
    } else {
        Game::S3
    }
}

impl Game {
    /// The game's name
    pub fn name(self) -> &'static str {
        match self {
            Game::S2 => "Splatoon 2",
            Game::S3 => "Splatoon 3",
        }
    }

    /// How the model sees it in a source label (`Splatoon 2 era`)
    pub fn era_label(self) -> &'static str {
        match self {
            Game::S2 => "Splatoon 2 era",
            Game::S3 => "Splatoon 3 era",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eras_split_at_the_launch_day() {
        let at = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        assert_eq!(era(at("2020-09-05T10:00:00Z")), Game::S2);
        assert_eq!(era(at("2022-09-08T23:59:59Z")), Game::S2);
        assert_eq!(era(at("2022-09-09T00:00:00Z")), Game::S3);
        assert_eq!(era(at("2026-09-26T00:00:00Z")), Game::S3);
        assert_eq!(serde_json::to_string(&Game::S2).unwrap(), "\"S2\"");
        assert_eq!(Game::S3.era_label(), "Splatoon 3 era");
    }
}
