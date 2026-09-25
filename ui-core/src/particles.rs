//! CPU particle emitters for effects such as engine exhaust, sparks and smoke.
//!
//! An emitter owns a fixed-capacity pool of live particles. Each frame the
//! game calls [`ParticleEmitter::update`] with the frame's delta and, while
//! the effect is on, an [`Emission`] describing where the source is, which way
//! it fires and how hard. [`ParticleEmitter::append_particles`] then writes the
//! live particles into a caller-owned buffer for a
//! [`PaintOp::ParticleBatch`](crate::paint::PaintOp::ParticleBatch).
//!
//! Nothing here allocates after construction: the pool is reserved once at
//! `capacity` and new particles are dropped when it is full, and the output
//! buffer belongs to the caller so it can be reused frame to frame.
//!
//! Particles spawned during one frame are spread along the path the source
//! took during that frame and pre-aged by their share of the frame, so a
//! fast-moving source leaves a continuous trail rather than clumps at
//! frame-rate intervals.
//!
//! Emitters keep simulating while [`ParticleEmitter::is_active`] is true;
//! a host driven by `FrameDemand` should keep requesting frames until it
//! turns false, then go idle.

use crate::geometry::{Color, Point, Scalar};
use crate::paint::Particle;

/// Static description of how an emitter's particles look and move.
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleEmitterConfig {
    /// Most particles alive at once. Spawns beyond this are dropped. Size it
    /// to roughly `peak rate * max lifetime`.
    pub capacity: usize,
    /// Lifetime range in seconds; each particle picks uniformly within it.
    pub lifetime: (f32, f32),
    /// Launch speed range in units per second along the emission direction.
    pub speed: (f32, f32),
    /// Half-angle of the launch cone, in radians.
    pub spread: f32,
    /// Fraction of the source's own velocity each particle starts with.
    pub inherit_velocity: f32,
    /// Exponential velocity decay rate, per second (0 = none).
    pub drag: f32,
    /// Radius at birth and at death; interpolated linearly over life.
    pub start_radius: Scalar,
    pub end_radius: Scalar,
    /// Color gradient over life, evenly spaced from birth to death. One stop
    /// means a constant color. Alpha is interpolated like the other channels,
    /// so ending on a transparent stop fades particles out.
    pub colors: Vec<Color>,
}

/// What the source is doing this frame. Pass `None` to
/// [`ParticleEmitter::update`] when the effect is off; live particles keep
/// moving and fading either way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Emission {
    /// Where particles are born this frame.
    pub origin: Point,
    /// Direction particles fly, in the same space as `origin`. Need not be
    /// normalized; a zero vector emits nothing.
    pub direction: Point,
    /// The source's own velocity, blended in by
    /// [`ParticleEmitterConfig::inherit_velocity`].
    pub source_velocity: Point,
    /// Particles per second.
    pub rate: f32,
    /// Scales launch speed and radius, typically `0.0..=1.0` (for example a
    /// throttle setting). Zero emits nothing.
    pub intensity: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LiveParticle {
    position: Point,
    velocity: Point,
    age: f32,
    lifetime: f32,
    size_scale: f32,
}

/// A fixed-capacity particle emitter. See the module docs.
#[derive(Debug, Clone)]
pub struct ParticleEmitter {
    config: ParticleEmitterConfig,
    particles: Vec<LiveParticle>,
    /// Fractional particles owed from previous frames, so a rate below the
    /// frame rate still emits at the right average.
    spawn_debt: f32,
    /// Where the source was at the end of the previous emitting frame.
    previous_origin: Option<Point>,
    rng: XorShift64,
}

impl ParticleEmitter {
    /// `seed` makes the effect reproducible; any value works.
    pub fn new(config: ParticleEmitterConfig, seed: u64) -> Self {
        let particles = Vec::with_capacity(config.capacity);
        Self {
            config,
            particles,
            spawn_debt: 0.0,
            previous_origin: None,
            rng: XorShift64::new(seed),
        }
    }

    pub fn config(&self) -> &ParticleEmitterConfig {
        &self.config
    }

    /// Number of live particles.
    pub fn len(&self) -> usize {
        self.particles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.particles.is_empty()
    }

    /// True while any particle is alive, i.e. while the effect still needs
    /// frames even if nothing is emitting.
    pub fn is_active(&self) -> bool {
        !self.particles.is_empty()
    }

    /// Removes every particle and forgets the previous source position, for
    /// example when the source teleports.
    pub fn clear(&mut self) {
        self.particles.clear();
        self.spawn_debt = 0.0;
        self.previous_origin = None;
    }

    /// Advances live particles by `dt` seconds, retires expired ones, then
    /// spawns this frame's new particles if `emission` is `Some`.
    pub fn update(&mut self, dt: f32, emission: Option<Emission>) {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        let damping = (-self.config.drag.max(0.0) * dt).exp();
        for particle in &mut self.particles {
            particle.age += dt;
            particle.position.x += particle.velocity.x * dt;
            particle.position.y += particle.velocity.y * dt;
            particle.velocity.x *= damping;
            particle.velocity.y *= damping;
        }
        // Order-preserving so newer particles stay later in the list and
        // draw over older ones.
        self.particles
            .retain(|particle| particle.age < particle.lifetime);

        match emission {
            Some(emission) => self.spawn(dt, emission),
            None => {
                self.spawn_debt = 0.0;
                self.previous_origin = None;
            }
        }
    }

    fn spawn(&mut self, dt: f32, emission: Emission) {
        let length = (emission.direction.x * emission.direction.x
            + emission.direction.y * emission.direction.y)
            .sqrt();
        let intensity = emission.intensity.max(0.0);
        let rate = emission.rate.max(0.0);
        let from = self.previous_origin.unwrap_or(emission.origin);
        self.previous_origin = Some(emission.origin);
        if !length.is_normal() || intensity <= 0.0 || rate <= 0.0 || dt <= 0.0 {
            self.spawn_debt = 0.0;
            return;
        }
        let base_angle = emission.direction.y.atan2(emission.direction.x);

        self.spawn_debt += rate * dt;
        let count = self.spawn_debt.floor();
        self.spawn_debt -= count;
        let count = count as usize;
        let config = &self.config;
        let free = config.capacity.saturating_sub(self.particles.len());
        for index in 0..count.min(free) {
            // Birth time within this frame, 0 = start, 1 = end.
            let birth = (index as f32 + self.rng.next_f32()) / count as f32;
            let age = dt * (1.0 - birth);
            let angle = base_angle + (self.rng.next_f32() * 2.0 - 1.0) * config.spread;
            let speed = lerp(config.speed.0, config.speed.1, self.rng.next_f32()) * intensity;
            let velocity = Point {
                x: angle.cos() * speed + emission.source_velocity.x * config.inherit_velocity,
                y: angle.sin() * speed + emission.source_velocity.y * config.inherit_velocity,
            };
            let lifetime = lerp(config.lifetime.0, config.lifetime.1, self.rng.next_f32());
            if lifetime <= age {
                continue;
            }
            let origin = Point {
                x: lerp(from.x, emission.origin.x, birth),
                y: lerp(from.y, emission.origin.y, birth),
            };
            self.particles.push(LiveParticle {
                position: Point {
                    x: origin.x + velocity.x * age,
                    y: origin.y + velocity.y * age,
                },
                velocity,
                age,
                lifetime,
                size_scale: intensity.clamp(0.25, 1.0),
            });
        }
    }

    /// Appends every live particle, oldest first, to `out`.
    pub fn append_particles(&self, out: &mut Vec<Particle>) {
        out.reserve(self.particles.len());
        for particle in &self.particles {
            let t = (particle.age / particle.lifetime).clamp(0.0, 1.0);
            let radius =
                lerp(self.config.start_radius, self.config.end_radius, t) * particle.size_scale;
            out.push(Particle {
                center: particle.position,
                radius: radius.max(0.0),
                color: sample_gradient(&self.config.colors, t),
            });
        }
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Evenly spaced gradient lookup; `t` in `0.0..=1.0`.
fn sample_gradient(stops: &[Color], t: f32) -> Color {
    match stops {
        [] => Color::rgba(0xff, 0xff, 0xff, 0xff),
        [only] => *only,
        _ => {
            let scaled = t.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
            let index = (scaled.floor() as usize).min(stops.len() - 2);
            let local = scaled - index as f32;
            let (a, b) = (stops[index], stops[index + 1]);
            let channel = |x: u8, y: u8| lerp(x as f32, y as f32, local).round() as u8;
            Color::rgba(
                channel(a.r, b.r),
                channel(a.g, b.g),
                channel(a.b, b.b),
                channel(a.a, b.a),
            )
        }
    }
}

/// xorshift64*: small, fast and plenty for visual jitter.
#[derive(Debug, Clone, Copy)]
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        // Zero is the one state xorshift cannot leave.
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Uniform in `[0, 1)`.
    fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ParticleEmitterConfig {
        ParticleEmitterConfig {
            capacity: 64,
            lifetime: (0.5, 0.5),
            speed: (100.0, 100.0),
            spread: 0.0,
            inherit_velocity: 0.0,
            drag: 0.0,
            start_radius: 4.0,
            end_radius: 0.0,
            colors: vec![Color::rgba(255, 255, 255, 255), Color::rgba(255, 0, 0, 0)],
        }
    }

    fn emission(rate: f32) -> Emission {
        Emission {
            origin: Point { x: 0.0, y: 0.0 },
            direction: Point { x: 1.0, y: 0.0 },
            source_velocity: Point { x: 0.0, y: 0.0 },
            rate,
            intensity: 1.0,
        }
    }

    #[test]
    fn emits_at_the_requested_average_rate_below_the_frame_rate() {
        let mut emitter = ParticleEmitter::new(config(), 1);
        // 10 particles/s at 60 fps is one particle every six frames.
        for _ in 0..15 {
            emitter.update(1.0 / 60.0, Some(emission(10.0)));
        }
        assert_eq!(emitter.len(), 2);
    }

    #[test]
    fn particles_expire_after_their_lifetime_and_the_emitter_goes_idle() {
        let mut emitter = ParticleEmitter::new(config(), 1);
        emitter.update(0.1, Some(emission(100.0)));
        assert!(emitter.is_active());
        for _ in 0..6 {
            emitter.update(0.1, None);
        }
        assert!(!emitter.is_active());
    }

    #[test]
    fn zero_intensity_or_rate_emits_nothing() {
        let mut emitter = ParticleEmitter::new(config(), 1);
        let mut idle = emission(100.0);
        idle.intensity = 0.0;
        emitter.update(0.1, Some(idle));
        emitter.update(0.1, Some(emission(0.0)));
        assert!(emitter.is_empty());
    }

    #[test]
    fn capacity_bounds_the_pool_without_growing_it() {
        let mut emitter = ParticleEmitter::new(config(), 1);
        let reserved = emitter.particles.capacity();
        emitter.update(0.1, Some(emission(10_000.0)));
        assert_eq!(emitter.len(), 64);
        assert_eq!(emitter.particles.capacity(), reserved);
    }

    #[test]
    fn particles_fly_along_the_emission_direction() {
        let mut emitter = ParticleEmitter::new(config(), 7);
        emitter.update(0.1, Some(emission(100.0)));
        emitter.update(0.1, None);
        let mut out = Vec::new();
        emitter.append_particles(&mut out);
        assert!(!out.is_empty());
        for particle in &out {
            assert!(particle.center.x > 0.0, "{particle:?}");
            assert!(particle.center.y.abs() < 1e-3, "{particle:?}");
        }
    }

    #[test]
    fn a_moving_source_leaves_a_trail_instead_of_a_clump() {
        let mut emitter = ParticleEmitter::new(
            ParticleEmitterConfig {
                speed: (0.0, 0.0),
                ..config()
            },
            3,
        );
        emitter.update(1.0 / 60.0, Some(emission(600.0)));
        let mut moved = emission(600.0);
        moved.origin = Point { x: 100.0, y: 0.0 };
        emitter.update(1.0 / 60.0, Some(moved));
        let mut out = Vec::new();
        emitter.append_particles(&mut out);
        let newest: Vec<f32> = out.iter().rev().take(10).map(|p| p.center.x).collect();
        let spread = newest.iter().cloned().fold(f32::MIN, f32::max)
            - newest.iter().cloned().fold(f32::MAX, f32::min);
        assert!(spread > 50.0, "newest particles bunched: {newest:?}");
    }

    #[test]
    fn color_and_radius_follow_the_life_curve() {
        let mut emitter = ParticleEmitter::new(config(), 1);
        emitter.update(0.0001, Some(emission(20_000.0)));
        let mut out = Vec::new();
        emitter.append_particles(&mut out);
        let young = out.last().expect("spawned");
        assert!(young.color.a > 250 && young.radius > 3.9, "{young:?}");

        emitter.update(0.45, None);
        out.clear();
        emitter.append_particles(&mut out);
        let old = out.last().expect("still alive");
        assert!(old.color.a < 30 && old.radius < 0.5, "{old:?}");
    }

    #[test]
    fn gradient_hits_each_stop_exactly() {
        let stops = [
            Color::rgba(0, 0, 0, 0),
            Color::rgba(100, 100, 100, 100),
            Color::rgba(200, 200, 200, 200),
        ];
        assert_eq!(sample_gradient(&stops, 0.0), stops[0]);
        assert_eq!(sample_gradient(&stops, 0.5), stops[1]);
        assert_eq!(sample_gradient(&stops, 1.0), stops[2]);
        assert_eq!(sample_gradient(&stops, 0.25), Color::rgba(50, 50, 50, 50));
    }

    #[test]
    fn same_seed_same_particles() {
        let run = || {
            let mut emitter = ParticleEmitter::new(
                ParticleEmitterConfig {
                    spread: 0.5,
                    speed: (50.0, 150.0),
                    lifetime: (0.2, 0.6),
                    ..config()
                },
                42,
            );
            for _ in 0..10 {
                emitter.update(1.0 / 60.0, Some(emission(200.0)));
            }
            let mut out = Vec::new();
            emitter.append_particles(&mut out);
            out
        };
        assert_eq!(run(), run());
    }
}
