//! GPU time per frame from `EXT_disjoint_timer_query_webgl2`, read the way a typical three.js perf
//! readout reads it, so the two compare: a `TIME_ELAPSED` query around the frame's GL work, a flush
//! right after it ends, and the newest result that has come back. Results arrive a few frames late
//! and never block.
//!
//! The queries are made once, at start-up, and reused from a ring: a frame creates no query
//! objects.

use crate::gl::Gl;

/// Queries in flight at most; a frame that finds the ring full goes untimed.
pub const RING: usize = 8;

pub struct GpuTimer {
    queries: [u32; RING],
    frames: [u64; RING],
    head: usize,
    pending: usize,
    active: bool,
    supported: bool,
    last_ms: f64,
    last_frame: u64,
}

impl Default for GpuTimer {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuTimer {
    pub const fn new() -> Self {
        Self {
            queries: [0; RING],
            frames: [0; RING],
            head: 0,
            pending: 0,
            active: false,
            supported: false,
            last_ms: f64::NAN,
            last_frame: 0,
        }
    }

    /// Creates the query ring if the extension is there.
    pub fn init<G: Gl>(&mut self, gl: &mut G) {
        self.supported = gl.timer_query_supported();
        if !self.supported {
            return;
        }
        for q in &mut self.queries {
            *q = gl.query_create();
        }
        self.supported = self.queries.iter().all(|&q| q != 0);
    }

    pub fn supported(&self) -> bool {
        self.supported
    }

    /// Collects every result that has come back, oldest first; a disjoint event (the GPU clock
    /// jumped, as on a throttling laptop) discards the batch. The disjoint flag is read only once a
    /// result is available, as the extension prescribes: reading it clears it, so reading it
    /// earlier would lose an event that spoils results still in flight.
    pub fn collect<G: Gl>(&mut self, gl: &mut G) {
        let mut disjoint = None;
        while self.pending > 0 {
            let q = self.queries[self.head];
            if !gl.query_available(q) {
                break;
            }
            let disjoint = *disjoint.get_or_insert_with(|| gl.gpu_disjoint());
            let ms = gl.query_result_ns(q) / 1e6;
            if !disjoint && ms.is_finite() {
                self.last_ms = ms;
                self.last_frame = self.frames[self.head];
            }
            self.head = (self.head + 1) % RING;
            self.pending -= 1;
        }
    }

    /// Starts timing frame `frame`, if a query is free.
    pub fn begin<G: Gl>(&mut self, gl: &mut G, frame: u64) {
        if !self.supported || self.active || self.pending == RING {
            return;
        }
        let slot = (self.head + self.pending) % RING;
        self.frames[slot] = frame;
        gl.query_begin(self.queries[slot]);
        self.active = true;
    }

    /// Ends the frame's query and flushes, so the end reaches the GPU now rather than when the
    /// whole animation-frame task is over (which would count later callbacks' time as well).
    pub fn end<G: Gl>(&mut self, gl: &mut G) {
        if !self.active {
            return;
        }
        gl.query_end();
        gl.flush();
        self.active = false;
        self.pending += 1;
    }

    /// The newest GPU time in milliseconds (NaN before the first), and the frame it measured.
    pub fn last(&self) -> (f64, u64) {
        (self.last_ms, self.last_frame)
    }

    /// Deletes the queries.
    pub fn release<G: Gl>(&mut self, gl: &mut G) {
        for q in &mut self.queries {
            if *q != 0 {
                gl.query_delete(*q);
                *q = 0;
            }
        }
        self.supported = false;
        self.pending = 0;
        self.active = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Call, MockGl};

    #[test]
    fn unsupported_timer_issues_no_queries() {
        let mut gl = MockGl::new();
        gl.timer = false;
        let mut t = GpuTimer::new();
        t.init(&mut gl);
        t.collect(&mut gl);
        t.begin(&mut gl, 1);
        t.end(&mut gl);
        assert!(!t.supported());
        assert!(gl.calls.is_empty(), "{:?}", gl.calls);
        assert!(t.last().0.is_nan());
    }

    #[test]
    fn results_arrive_late_and_in_order() {
        let mut gl = MockGl::new();
        let mut t = GpuTimer::new();
        t.init(&mut gl);
        gl.query_ready = false;
        for frame in 1..=3 {
            t.collect(&mut gl);
            t.begin(&mut gl, frame);
            t.end(&mut gl);
        }
        assert!(t.last().0.is_nan(), "nothing back yet");
        assert!(gl.calls.contains(&Call::Flush), "end flushes");
        gl.query_ready = true;
        gl.query_ms = 4.5;
        t.collect(&mut gl);
        assert_eq!(t.last(), (4.5, 3), "newest of the three");
    }

    #[test]
    fn disjoint_discards_the_batch() {
        let mut gl = MockGl::new();
        let mut t = GpuTimer::new();
        t.init(&mut gl);
        gl.query_ms = 2.0;
        t.begin(&mut gl, 1);
        t.end(&mut gl);
        t.collect(&mut gl);
        assert_eq!(t.last(), (2.0, 1));
        gl.disjoint = true;
        gl.query_ms = 99.0;
        t.begin(&mut gl, 2);
        t.end(&mut gl);
        t.collect(&mut gl);
        assert_eq!(t.last(), (2.0, 1), "the disjoint result is dropped");
    }

    #[test]
    fn the_disjoint_flag_is_read_only_once_a_result_is_in() {
        let mut gl = MockGl::new();
        let mut t = GpuTimer::new();
        t.init(&mut gl);
        gl.query_ready = false;
        t.begin(&mut gl, 1);
        t.end(&mut gl);
        // A disjoint event while the query is in flight: collecting now must not consume it.
        gl.disjoint = true;
        t.collect(&mut gl);
        assert!(
            !gl.calls.contains(&Call::Disjoint),
            "read before any result was in"
        );
        gl.query_ready = true;
        gl.query_ms = 7.0;
        t.collect(&mut gl);
        assert_eq!(gl.calls.iter().filter(|c| **c == Call::Disjoint).count(), 1);
        assert!(t.last().0.is_nan(), "the spoilt result was discarded");
    }

    #[test]
    fn a_full_ring_skips_frames_instead_of_blocking() {
        let mut gl = MockGl::new();
        let mut t = GpuTimer::new();
        t.init(&mut gl);
        gl.query_ready = false;
        for frame in 1..=(RING as u64 + 3) {
            t.collect(&mut gl);
            t.begin(&mut gl, frame);
            t.end(&mut gl);
        }
        let begun = gl
            .calls
            .iter()
            .filter(|c| matches!(c, Call::QueryBegin(_)))
            .count();
        assert_eq!(begun, RING);
        gl.query_ready = true;
        t.collect(&mut gl);
        assert_eq!(t.last().1, RING as u64);
        // The ring is free again.
        t.begin(&mut gl, 100);
        t.end(&mut gl);
        t.collect(&mut gl);
        assert_eq!(t.last().1, 100);
    }
}
