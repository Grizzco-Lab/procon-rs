//! Multi-object tracking in the manner of SORT: predict each track with
//! constant velocity, match predictions to detections by IoU with the
//! Hungarian algorithm, then correct.
//!
//! Each track keeps a box (center x, center y, width, height, in frame
//! fractions) and its change per frame. A match corrects both with an
//! alpha-beta filter, a fixed-gain Kalman filter:
//! `box = predicted + alpha * residual` and
//! `velocity += beta * residual / frames`. Detections match
//! only tracks of their class. An unmatched detection starts a tentative
//! track, which is confirmed and given an id after `min_hits` matches in a
//! row and dropped on its first miss; a confirmed track survives `max_age`
//! frames without a match, coasting on its velocity.

use crate::labels::{FrameObjects, ObjectBox, Source, iou};
use alloc::string::String;
use alloc::vec::Vec;

/// Tracker settings
#[derive(Clone, Copy, Debug)]
pub struct TrackerConfig {
    /// Lowest IoU between a prediction and a detection that matches
    pub min_iou: f64,
    /// Matches in a row before a track is confirmed and output
    pub min_hits: u32,
    /// Frames a confirmed track lives without a match
    pub max_age: u64,
    /// Gain on the box residual
    pub alpha: f64,
    /// Gain on the velocity residual
    pub beta: f64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            min_iou: 0.3,
            min_hits: 3,
            max_age: 15,
            alpha: 0.6,
            beta: 0.2,
        }
    }
}

/// One tracked object
#[derive(Clone, Debug)]
struct Track {
    /// Assigned when confirmed
    id: Option<u64>,
    class: String,
    /// Center x, center y, width, height
    state: [f64; 4],
    /// Change of `state` per frame
    velocity: [f64; 4],
    last_frame: u64,
    hits: u32,
}

impl Track {
    /// The box at `frame` if it keeps its velocity
    fn predict(&self, frame: u64) -> [f64; 4] {
        let dt = frame.saturating_sub(self.last_frame) as f64;
        let mut s = core::array::from_fn(|i| self.state[i] + self.velocity[i] * dt);
        s[2] = f64::max(s[2], 1e-4);
        s[3] = f64::max(s[3], 1e-4);
        s
    }
}

/// `[x, y, w, h]` to `[cx, cy, w, h]`
fn center([x, y, w, h]: [f64; 4]) -> [f64; 4] {
    [x + w / 2.0, y + h / 2.0, w, h]
}

/// `[cx, cy, w, h]` to `[x, y, w, h]`
fn corner([cx, cy, w, h]: [f64; 4]) -> [f64; 4] {
    [cx - w / 2.0, cy - h / 2.0, w, h]
}

/// Tracks objects over a sequence of frames
#[derive(Debug)]
pub struct Tracker {
    config: TrackerConfig,
    tracks: Vec<Track>,
    next_id: u64,
}

impl Tracker {
    /// A tracker with no tracks; ids start at 1
    pub fn new(config: TrackerConfig) -> Self {
        Self {
            config,
            tracks: Vec::new(),
            next_id: 1,
        }
    }

    /// Feed the detections of `frame` (frames in increasing order, gaps
    /// allowed) and get the confirmed tracks matched in it, with their
    /// filtered boxes and ids
    pub fn update(&mut self, frame: u64, detections: &[ObjectBox]) -> Vec<ObjectBox> {
        let cfg = self.config;
        // Too long unseen, even if this frame has a match for it
        self.tracks
            .retain(|t| frame.saturating_sub(t.last_frame) <= cfg.max_age);
        let predicted: Vec<[f64; 4]> = self.tracks.iter().map(|t| t.predict(frame)).collect();
        // Different classes never match: cost 1, as for no overlap
        let cost: Vec<Vec<f64>> = self
            .tracks
            .iter()
            .zip(&predicted)
            .map(|(t, p)| {
                detections
                    .iter()
                    .map(|d| {
                        if d.class == t.class {
                            1.0 - iou(corner(*p), d.rect())
                        } else {
                            1.0
                        }
                    })
                    .collect()
            })
            .collect();
        let assignment = hungarian(&cost, detections.len());

        let mut matched_detection = vec![false; detections.len()];
        let mut output = Vec::new();
        let mut keep = Vec::with_capacity(self.tracks.len());
        for (i, track) in self.tracks.iter_mut().enumerate() {
            let det = assignment[i].filter(|&j| 1.0 - cost[i][j] >= cfg.min_iou);
            let Some(j) = det else {
                // Tentative tracks die on a miss, confirmed ones when too old
                let alive =
                    track.id.is_some() && frame.saturating_sub(track.last_frame) <= cfg.max_age;
                keep.push(alive);
                continue;
            };
            matched_detection[j] = true;
            let z = center(detections[j].rect());
            let dt = frame.saturating_sub(track.last_frame).max(1) as f64;
            for k in 0..4 {
                let residual = z[k] - predicted[i][k];
                track.state[k] = predicted[i][k] + cfg.alpha * residual;
                track.velocity[k] += cfg.beta * residual / dt;
            }
            track.last_frame = frame;
            track.hits += 1;
            if track.id.is_none() && track.hits >= cfg.min_hits {
                track.id = Some(self.next_id);
                self.next_id += 1;
            }
            if track.id.is_some() {
                output.push(ObjectBox {
                    id: track.id,
                    by: Source::Model,
                    ..ObjectBox::model(
                        &track.class,
                        corner(track.state),
                        detections[j].score.unwrap_or(1.0),
                    )
                });
            }
            keep.push(true);
        }
        let mut keep = keep.into_iter();
        self.tracks.retain(|_| keep.next().unwrap());

        for (d, _) in detections
            .iter()
            .zip(&matched_detection)
            .filter(|(_, m)| !**m)
        {
            self.tracks.push(Track {
                id: None,
                class: d.class.clone(),
                state: center(d.rect()),
                velocity: [0.0; 4],
                last_frame: frame,
                hits: 1,
            });
            // A single hit confirms when `min_hits` is 1
            if cfg.min_hits <= 1 {
                let t = self.tracks.last_mut().unwrap();
                t.id = Some(self.next_id);
                self.next_id += 1;
                output.push(ObjectBox {
                    id: t.id,
                    ..d.clone()
                });
            }
        }
        output
    }
}

/// Track every frame of a detection sequence; frames without confirmed
/// tracks are left out
pub fn track_frames(frames: &[FrameObjects], config: TrackerConfig) -> Vec<FrameObjects> {
    let mut sorted: Vec<&FrameObjects> = frames.iter().collect();
    sorted.sort_by_key(|f| f.frame);
    let mut tracker = Tracker::new(config);
    sorted
        .into_iter()
        .map(|f| FrameObjects::new(f.frame, tracker.update(f.frame, &f.boxes)))
        .filter(|f| !f.boxes.is_empty())
        .collect()
}

/// Minimum-cost assignment of rows to columns (the Hungarian algorithm,
/// O(n³) with potentials). `cost` has one row per worker and `columns`
/// entries per row; the result gives each row its column, or `None` when
/// there are more rows than columns and the row is left out.
pub fn hungarian(cost: &[Vec<f64>], columns: usize) -> Vec<Option<usize>> {
    let rows = cost.len();
    let n = rows.max(columns);
    if n == 0 {
        return Vec::new();
    }
    // Padded square matrix; padding costs the same as no match
    let c = |i: usize, j: usize| {
        if i < rows && j < columns {
            cost[i][j]
        } else {
            1.0
        }
    };
    // 1-based: u, v potentials; p[j] the row matched to column j
    let mut u = vec![0.0; n + 1];
    let mut v = vec![0.0; n + 1];
    let mut p = vec![0usize; n + 1];
    let mut way = vec![0usize; n + 1];
    for i in 1..=n {
        p[0] = i;
        let mut j0 = 0;
        let mut minv = vec![f64::INFINITY; n + 1];
        let mut used = vec![false; n + 1];
        loop {
            used[j0] = true;
            let i0 = p[j0];
            let mut delta = f64::INFINITY;
            let mut j1 = 0;
            for j in 1..=n {
                if !used[j] {
                    let cur = c(i0 - 1, j - 1) - u[i0] - v[j];
                    if cur < minv[j] {
                        minv[j] = cur;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=n {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut assignment = vec![None; rows];
    for j in 1..=n {
        if p[j] >= 1 && p[j] <= rows && j <= columns {
            assignment[p[j] - 1] = Some(j - 1);
        }
    }
    assignment
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(class: &str, x: f64, y: f64) -> ObjectBox {
        ObjectBox::model(class, [x, y, 0.1, 0.1], 0.9)
    }

    #[test]
    fn hungarian_beats_greedy() {
        // Greedy takes (0, 0) at 1 and then pays 10; optimal is 2 + 2
        let cost = vec![vec![1.0, 2.0], vec![2.0, 10.0]];
        assert_eq!(hungarian(&cost, 2), [Some(1), Some(0)]);
    }

    #[test]
    fn hungarian_rectangular() {
        let cost = vec![vec![0.5], vec![0.1], vec![0.9]];
        assert_eq!(hungarian(&cost, 1), [None, Some(0), None]);
        let cost = vec![vec![0.9, 0.2, 0.5]];
        assert_eq!(hungarian(&cost, 3), [Some(1)]);
        assert!(hungarian(&[], 0).is_empty());
        assert_eq!(hungarian(&[vec![], vec![]], 0), [None, None]);
    }

    #[test]
    fn confirms_after_min_hits_and_keeps_ids() {
        let mut t = Tracker::new(TrackerConfig::default());
        assert!(t.update(0, &[det("chum", 0.1, 0.1)]).is_empty());
        assert!(t.update(1, &[det("chum", 0.11, 0.1)]).is_empty());
        let out = t.update(2, &[det("chum", 0.12, 0.1), det("maws", 0.6, 0.6)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, Some(1));
        let out = t.update(3, &[det("chum", 0.13, 0.1)]);
        assert_eq!(out[0].id, Some(1));
    }

    #[test]
    fn constant_velocity_bridges_a_gap() {
        let cfg = TrackerConfig {
            min_iou: 0.3,
            ..Default::default()
        };
        let mut t = Tracker::new(cfg);
        // Moves 0.03 per frame, a box 0.1 wide
        for f in 0..10 {
            t.update(f, &[det("steelhead", 0.03 * f as f64, 0.4)]);
        }
        // Missing for 4 frames: the object moved 0.15, more than a box
        // width, so without prediction the IoU would be zero
        let out = t.update(14, &[det("steelhead", 0.42, 0.4)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, Some(1));
    }

    #[test]
    fn classes_do_not_mix_and_old_tracks_die() {
        let cfg = TrackerConfig {
            min_hits: 1,
            max_age: 2,
            ..Default::default()
        };
        let mut t = Tracker::new(cfg);
        assert_eq!(t.update(0, &[det("chum", 0.1, 0.1)])[0].id, Some(1));
        // Same place, other class: a new track
        assert_eq!(t.update(1, &[det("cohock", 0.1, 0.1)])[0].id, Some(2));
        // Chum is back within max_age
        let out = t.update(2, &[det("chum", 0.1, 0.1)]);
        assert_eq!(out[0].id, Some(1));
        // Chum returns after more than max_age frames: a new id
        let out = t.update(6, &[det("chum", 0.1, 0.1)]);
        assert_eq!(out[0].id, Some(3));
    }

    #[test]
    fn tentative_tracks_die_on_a_miss() {
        let mut t = Tracker::new(TrackerConfig::default());
        t.update(0, &[det("chum", 0.1, 0.1)]);
        t.update(1, &[det("chum", 0.1, 0.1)]);
        t.update(2, &[]);
        t.update(3, &[det("chum", 0.1, 0.1)]);
        // Only two hits in a row since the miss
        assert!(t.update(4, &[det("chum", 0.1, 0.1)]).is_empty());
        assert_eq!(t.update(5, &[det("chum", 0.1, 0.1)])[0].id, Some(1));
    }

    #[test]
    fn track_frames_skips_empty() {
        let frames: Vec<FrameObjects> = (0..5)
            .map(|f| FrameObjects::new(f, vec![det("maws", 0.5, 0.5)]))
            .collect();
        let tracks = track_frames(&frames, TrackerConfig::default());
        assert_eq!(
            tracks.iter().map(|f| f.frame).collect::<Vec<_>>(),
            [2, 3, 4]
        );
        assert!(tracks.iter().all(|f| f.boxes[0].id == Some(1)));
    }
}
