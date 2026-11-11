//! Nearest-feature search over the database.
//!
//! Brute force over every frame, pruned by axis-aligned bounding boxes of the feature
//! vectors over runs of 64 and of 16 frames: a box farther from the query than the best
//! frame so far cannot hold a better one. Each frame's distance stops accumulating once it
//! passes the best. The result is exact, the same frame brute force finds.

use crate::database::{Database, FEATURES, LARGE_BOX, SMALL_BOX};

/// What the search may not return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exclusions {
    /// Frames within this many of their clip's end (playback needs room to continue).
    pub end_margin: usize,
    /// A range of frames to skip, `[start, end)`, such as those around the current one.
    pub skip: (usize, usize),
    /// Only frames of clips with one of these tags (`database::tags`) ...
    pub tags: u32,
    /// ... and none of these.
    pub exclude: u32,
}

/// The best frame and its squared feature distance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Match {
    pub frame: usize,
    pub cost: f32,
}

/// Counts of what one search visited, for measuring the pruning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchStats {
    pub frames_tested: usize,
    pub small_boxes_tested: usize,
    pub large_boxes_tested: usize,
}

fn box_distance(query: &[f32; FEATURES], min: &[f32], max: &[f32], limit: f32, from: usize) -> f32 {
    let mut d = 0.0;
    for k in from..FEATURES {
        let q = query[k];
        let e = (min[k] - q).max(q - max[k]).max(0.0);
        d += e * e;
        if d >= limit {
            break;
        }
    }
    d
}

/// The frame nearest `query` with a cost below `best.cost` (pass the current frame's cost
/// to find only a better one, or `f32::INFINITY`). Allocates nothing.
#[must_use]
pub fn search(
    db: &Database,
    query: &[f32; FEATURES],
    best: Option<Match>,
    exclusions: Exclusions,
    stats: &mut SearchStats,
) -> Option<Match> {
    search_features(db, query, best, exclusions, stats, 0)
}

/// [`search`] over the features from `from` on only (the trajectory's, say, dropping the
/// pose's), through the same bounding boxes: a box's distance over those features alone is
/// still a lower bound. Allocates nothing.
#[must_use]
pub fn search_features(
    db: &Database,
    query: &[f32; FEATURES],
    mut best: Option<Match>,
    exclusions: Exclusions,
    stats: &mut SearchStats,
    from: usize,
) -> Option<Match> {
    let total = db.frame_count();
    let b = &db.bounds;
    let mut best_cost = best.map_or(f32::INFINITY, |m| m.cost);
    let mut large = 0;
    while large * LARGE_BOX < total {
        stats.large_boxes_tested += 1;
        let lo = large * FEATURES;
        if b.large_tags[large] & exclusions.tags == 0 {
            large += 1;
            continue;
        }
        if box_distance(
            query,
            &b.large_min[lo..lo + FEATURES],
            &b.large_max[lo..lo + FEATURES],
            best_cost,
            from,
        ) >= best_cost
        {
            large += 1;
            continue;
        }
        let first_small = large * (LARGE_BOX / SMALL_BOX);
        for small in first_small..(first_small + LARGE_BOX / SMALL_BOX) {
            let start = small * SMALL_BOX;
            if start >= total {
                break;
            }
            if b.small_tags[small] & exclusions.tags == 0 {
                continue;
            }
            stats.small_boxes_tested += 1;
            let so = small * FEATURES;
            if box_distance(
                query,
                &b.small_min[so..so + FEATURES],
                &b.small_max[so..so + FEATURES],
                best_cost,
                from,
            ) >= best_cost
            {
                continue;
            }
            let end = (start + SMALL_BOX).min(total);
            for f in start..end {
                if f >= exclusions.skip.0 && f < exclusions.skip.1 {
                    continue;
                }
                if f + exclusions.end_margin >= b.clip_end[f] as usize
                    || b.frame_tags[f] & exclusions.tags == 0
                    || b.frame_tags[f] & exclusions.exclude != 0
                {
                    continue;
                }
                stats.frames_tested += 1;
                let feat = &db.features[f * FEATURES..(f + 1) * FEATURES];
                let mut d = 0.0;
                for k in from..FEATURES {
                    let e = query[k] - feat[k];
                    d += e * e;
                    if d >= best_cost {
                        break;
                    }
                }
                if d < best_cost {
                    best_cost = d;
                    best = Some(Match { frame: f, cost: d });
                }
            }
        }
        large += 1;
    }
    best
}

/// The plain brute-force answer, for checking [`search`].
#[must_use]
pub fn search_brute_force(
    db: &Database,
    query: &[f32; FEATURES],
    exclusions: Exclusions,
) -> Option<Match> {
    let mut best: Option<Match> = None;
    for f in 0..db.frame_count() {
        if f >= exclusions.skip.0 && f < exclusions.skip.1 {
            continue;
        }
        if f + exclusions.end_margin >= db.bounds.clip_end[f] as usize
            || db.bounds.frame_tags[f] & exclusions.tags == 0
            || db.bounds.frame_tags[f] & exclusions.exclude != 0
        {
            continue;
        }
        let feat = db.feature(f);
        let d: f32 = (0..FEATURES).map(|k| (query[k] - feat[k]).powi(2)).sum();
        if best.is_none_or(|m| d < m.cost) {
            best = Some(Match { frame: f, cost: d });
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{BuildConfig, Database};
    use crate::testing::{humanoid_skeleton, synthetic_idle, synthetic_walk};

    #[test]
    fn pruned_search_finds_the_brute_force_frame() {
        let target = humanoid_skeleton(1.0);
        let clips = [
            synthetic_walk(&target, 400, 0.8),
            synthetic_walk(&target, 300, 1.3),
            synthetic_idle(&target, 200),
        ];
        let db = Database::build(&target, &clips, &BuildConfig::default()).unwrap();
        let ex = Exclusions {
            end_margin: 20,
            skip: (0, 0),
            tags: crate::database::tags::LOCOMOTION,
            exclude: 0,
        };
        let mut tested = 0;
        for probe in (0..db.frame_count()).step_by(37) {
            let mut q = [0.0f32; FEATURES];
            q.copy_from_slice(db.feature(probe));
            for (k, v) in q.iter_mut().enumerate() {
                *v += ((probe * 31 + k * 17) % 13) as f32 * 0.05 - 0.3;
            }
            let mut stats = SearchStats::default();
            let a = search(&db, &q, None, ex, &mut stats).unwrap();
            let b = search_brute_force(&db, &q, ex).unwrap();
            assert!(
                (a.cost - b.cost).abs() < 1e-5,
                "probe {probe}: {a:?} vs {b:?}"
            );
            tested += stats.frames_tested;
        }
        assert!(tested > 0);
    }

    #[test]
    fn a_search_over_some_features_finds_their_brute_force_frame() {
        let target = humanoid_skeleton(1.0);
        let clips = [
            synthetic_walk(&target, 400, 0.8),
            synthetic_idle(&target, 200),
        ];
        let db = Database::build(&target, &clips, &BuildConfig::default()).unwrap();
        let ex = Exclusions {
            end_margin: 20,
            skip: (0, 0),
            tags: crate::database::tags::LOCOMOTION,
            exclude: 0,
        };
        for probe in (0..db.frame_count()).step_by(41) {
            let mut q = [0.0f32; FEATURES];
            q.copy_from_slice(db.feature(probe));
            // The pose's features far off: they must not count.
            for v in q.iter_mut().take(15) {
                *v += 40.0;
            }
            let mut stats = SearchStats::default();
            let a = search_features(&db, &q, None, ex, &mut stats, 15).unwrap();
            let mut best = (f32::INFINITY, 0);
            for f in 0..db.frame_count() {
                if f + ex.end_margin >= db.bounds.clip_end[f] as usize
                    || db.bounds.frame_tags[f] & ex.tags == 0
                {
                    continue;
                }
                let feat = db.feature(f);
                let d: f32 = (15..FEATURES).map(|k| (q[k] - feat[k]).powi(2)).sum();
                if d < best.0 {
                    best = (d, f);
                }
            }
            assert!(
                (a.cost - best.0).abs() < 1e-4,
                "probe {probe}: {a:?} vs {best:?}"
            );
        }
    }
}
