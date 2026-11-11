//! `MotionMatcher::step` allocates nothing: counted with this test binary's own allocator
//! over walking, turning, stopping and standing, searches and transitions included.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use loadngo_animation::controller::{Config, MotionMatcher};
use loadngo_animation::database::{BuildConfig, Database};
use loadngo_animation::math::Vec3;
use loadngo_animation::path::{ConstantSteering, PathSteering};
use loadngo_animation::testing::{humanoid_skeleton, synthetic_idle, synthetic_walk};

struct Counting;
static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn stepping_allocates_nothing() {
    let target = humanoid_skeleton(1.0);
    let clips = [
        synthetic_walk(&target, 600, 1.0),
        synthetic_walk(&target, 600, 0.7),
        synthetic_idle(&target, 300),
    ];
    let db = Arc::new(Database::build(&target, &clips, &BuildConfig::default()).unwrap());
    let mut m = MotionMatcher::new(db, Config::default(), Vec3::ZERO, 0.0);
    let path = PathSteering::new(
        &[
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, 4.0),
            Vec3::new(3.0, 0.0, 4.0),
        ],
        0.9,
    );
    let still = ConstantSteering {
        velocity: Vec3::ZERO,
        facing: 1.0,
    };
    let before = m.stats();
    COUNTING.store(true, Ordering::SeqCst);
    for i in 0..900 {
        if i < 700 {
            m.step(1.0 / 60.0, &path);
        } else {
            m.step(1.0 / 75.0, &still);
        }
    }
    COUNTING.store(false, Ordering::SeqCst);
    let after = m.stats();
    assert!(after.searches > before.searches + 50, "the run searched");
    assert!(
        after.transitions > before.transitions,
        "the run transitioned"
    );
    assert_eq!(
        ALLOCS.load(Ordering::SeqCst),
        0,
        "allocations inside step()"
    );
}
