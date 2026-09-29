//! `delta_rule_recurrence` against a float64 reference: one and many steps, several
//! heads, narrow shapes, state carried between dispatches, and shape validation.
#![cfg(target_os = "macos")]

use std::sync::{Arc, Mutex};

use loadngo_metal_compute::{Batch, Buffer, Completed, Dispatch, Gpu, RecurrenceShape, Slice};
use loadngo_proactor::{new_platform_proactor, PlatformPort, Proactor};

fn run(proactor: &Proactor<PlatformPort>, batch: Batch<'_>) -> Completed {
    let slot: Arc<Mutex<Option<Completed>>> = Arc::default();
    let filled = Arc::clone(&slot);
    batch.commit(&proactor.handle(), move |done| {
        *filled.lock().unwrap() = Some(done);
    });
    loop {
        proactor.run_once().unwrap();
        if let Some(done) = slot.lock().unwrap().take() {
            return done;
        }
    }
}

/// Small deterministic values in [-1, 1).
fn values(seed: u64, n: usize) -> Vec<f32> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        })
        .collect()
}

fn upload(gpu: &Gpu, v: &[f32]) -> Buffer {
    let mut buffer = gpu.buffer(v.len().max(1) * 4).unwrap();
    buffer.as_f32_mut()[..v.len()].copy_from_slice(v);
    buffer
}

struct Inputs {
    q: Vec<f32>,
    k: Vec<f32>,
    v: Vec<f32>,
    alpha: Vec<f32>,
    beta: Vec<f32>,
}

/// Inputs shaped like the model's: unit-length keys and queries, decay in (0.5, 1),
/// write strength in (0, 1).
fn inputs(seed: u64, s: RecurrenceShape) -> Inputs {
    let unit = |raw: Vec<f32>| -> Vec<f32> {
        raw.chunks(s.dk)
            .flat_map(|row| {
                let norm = row.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                row.iter().map(move |x| x / norm)
            })
            .collect()
    };
    Inputs {
        q: unit(values(seed, s.t * s.heads * s.dk)),
        k: unit(values(seed + 1, s.t * s.heads * s.dk)),
        v: values(seed + 2, s.t * s.heads * s.dv),
        alpha: values(seed + 3, s.t * s.heads * s.dk)
            .iter()
            .map(|x| 0.75 + 0.25 * x)
            .collect(),
        beta: values(seed + 4, s.t * s.heads)
            .iter()
            .map(|x| 0.5 + 0.5 * x)
            .collect(),
    }
}

/// The definition in float64; returns the outputs and updates `state`.
fn reference(x: &Inputs, state: &mut [f64], s: RecurrenceShape) -> Vec<f64> {
    let mut out = vec![0.0; s.t * s.heads * s.dv];
    for step in 0..s.t {
        for h in 0..s.heads {
            let at = step * s.heads + h;
            let st = &mut state[h * s.dk * s.dv..(h + 1) * s.dk * s.dv];
            let (k, q, a) = (
                &x.k[at * s.dk..][..s.dk],
                &x.q[at * s.dk..][..s.dk],
                &x.alpha[at * s.dk..][..s.dk],
            );
            for i in 0..s.dk {
                for j in 0..s.dv {
                    st[i * s.dv + j] *= f64::from(a[i]);
                }
            }
            for j in 0..s.dv {
                let u: f64 = (0..s.dk).map(|i| f64::from(k[i]) * st[i * s.dv + j]).sum();
                let delta = f64::from(x.beta[at]) * (f64::from(x.v[at * s.dv + j]) - u);
                for i in 0..s.dk {
                    st[i * s.dv + j] += f64::from(k[i]) * delta;
                }
            }
            for j in 0..s.dv {
                out[at * s.dv + j] = (0..s.dk).map(|i| f64::from(q[i]) * st[i * s.dv + j]).sum();
            }
        }
    }
    out
}

/// Runs the recurrence from `state` on the GPU; returns (outputs, state).
fn gpu_run(
    gpu: &Gpu,
    proactor: &Proactor<PlatformPort>,
    x: &Inputs,
    state: &[f32],
    s: RecurrenceShape,
) -> (Vec<f32>, Vec<f32>) {
    let parts = [&x.q, &x.k, &x.v, &x.alpha, &x.beta];
    let mut buffers: Vec<Buffer> = parts.iter().map(|p| upload(gpu, p)).collect();
    buffers.push(upload(gpu, state));
    buffers.push(gpu.buffer(s.t * s.heads * s.dv * 4).unwrap());
    let slice = |i: usize, floats: usize| Slice::new(i, 0, floats * 4);
    let mut batch = gpu.batch(buffers, Dispatch::Serial).unwrap();
    batch
        .delta_rule_recurrence(
            (
                slice(0, x.q.len()),
                slice(1, x.k.len()),
                slice(2, x.v.len()),
                slice(3, x.alpha.len()),
                slice(4, x.beta.len()),
            ),
            slice(5, state.len()),
            slice(6, s.t * s.heads * s.dv),
            s,
        )
        .unwrap();
    let done = run(proactor, batch);
    done.gpu_time.unwrap();
    (
        done.buffers[6].as_f32()[..s.t * s.heads * s.dv].to_vec(),
        done.buffers[5].as_f32()[..state.len()].to_vec(),
    )
}

/// Largest absolute difference; NaN counts as the worst.
fn max_error(got: &[f32], want: &[f64]) -> f64 {
    got.iter()
        .zip(want)
        .map(|(&g, &w)| (f64::from(g) - w).abs())
        .fold(0.0, |worst, e| {
            if e.is_nan() {
                f64::INFINITY
            } else {
                worst.max(e)
            }
        })
}

#[test]
fn matches_the_float64_definition_and_carries_state() {
    let gpu = Gpu::new().unwrap();
    let proactor = new_platform_proactor().unwrap();
    for (s, seed) in [
        (
            RecurrenceShape {
                t: 1,
                heads: 4,
                dk: 128,
                dv: 128,
            },
            10,
        ),
        (
            RecurrenceShape {
                t: 300,
                heads: 3,
                dk: 128,
                dv: 128,
            },
            20,
        ),
        (
            RecurrenceShape {
                t: 17,
                heads: 2,
                dk: 64,
                dv: 40,
            },
            30,
        ),
        (
            RecurrenceShape {
                t: 5,
                heads: 1,
                dk: 2,
                dv: 1,
            },
            40,
        ),
    ] {
        let start: Vec<f32> = values(seed + 9, s.heads * s.dk * s.dv)
            .iter()
            .map(|x| 0.1 * x)
            .collect();
        let mut want_state: Vec<f64> = start.iter().map(|&x| f64::from(x)).collect();
        // Two dispatches in a row: the state the first leaves is where the second starts.
        let mut state = start;
        for part in 0..2 {
            let x = inputs(seed + 100 * part, s);
            let want = reference(&x, &mut want_state, s);
            let (out, next) = gpu_run(&gpu, &proactor, &x, &state, s);
            let out_error = max_error(&out, &want);
            let state_error = max_error(&next, &want_state);
            assert!(
                out_error < 1e-4,
                "{s:?} part {part}: output error {out_error:e}"
            );
            assert!(
                state_error < 1e-4,
                "{s:?} part {part}: state error {state_error:e}"
            );
            state = next;
        }
    }
}

#[test]
fn rejects_unsupported_shapes_and_state_overlapping_an_input() {
    let gpu = Gpu::new().unwrap();
    let mut batch = gpu
        .batch(vec![gpu.buffer(1 << 20).unwrap()], Dispatch::Serial)
        .unwrap();
    let at = |offset: usize, floats: usize| Slice::new(0, offset * 4, floats * 4);
    // t 1, one head, dk 2, dv 2: q k v alpha 2 floats each, beta 1, state 4, out 2.
    let inputs = (at(0, 2), at(2, 2), at(4, 2), at(6, 2), at(8, 1));
    for shape in [
        RecurrenceShape {
            t: 1,
            heads: 1,
            dk: 3,
            dv: 2,
        },
        RecurrenceShape {
            t: 1,
            heads: 1,
            dk: 130,
            dv: 2,
        },
        RecurrenceShape {
            t: 1,
            heads: 1,
            dk: 2,
            dv: 129,
        },
        RecurrenceShape {
            t: 0,
            heads: 1,
            dk: 2,
            dv: 2,
        },
    ] {
        assert!(batch
            .delta_rule_recurrence(inputs, at(16, 4), at(32, 2), shape)
            .is_err());
    }
    let shape = RecurrenceShape {
        t: 1,
        heads: 1,
        dk: 2,
        dv: 2,
    };
    assert!(batch
        .delta_rule_recurrence(inputs, at(0, 4), at(32, 2), shape)
        .is_err());
    assert!(batch
        .delta_rule_recurrence(inputs, at(16, 4), at(32, 2), shape)
        .is_ok());
}
