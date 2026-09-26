//! Measures the Metal matrix-vector kernels on Kimi-Linear-sized weights and checks their
//! results against the CPU reference in `loadngo-weights`. Run with `--help`.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("metal_gemv_bench needs macOS (Metal)");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    if let Err(message) = bench::main() {
        eprintln!("metal_gemv_bench: {message}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "macos")]
mod bench {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use loadngo_metal_compute::{Batch, Buffer, Completed, Dispatch, Gpu, Rows, Slice};
    use loadngo_proactor::{new_platform_proactor, PlatformPort, Proactor};
    use loadngo_weights::dense::{HalfFormat, HalfMatrix};
    use loadngo_weights::mxfp4::Mxfp4Matrix;

    const HELP: &str = "\
metal_gemv_bench: bandwidth and correctness of loadngo-metal-compute's GEMV kernels

Weights are random, in shapes approximating one Kimi-Linear-48B-A3B decode step:
  lm-head   the 163840x2304 bf16 output projection (one dispatch, 755 MB)
  trunk     27 layers of bf16 projections (4 attention, shared expert or
            the first layer's dense MLP) plus the LM head, ~3.3 GB
  experts   26 layers x 8 routed experts (gate, up, down) in MXFP4, ~0.8 GB,
            drawn from a larger pool so no two steps read the same experts
  token     trunk + experts in one command buffer, as a whole decode step

Each workload is timed over GPU start/end of its command buffer, for each
rows-per-simdgroup variant. The token workload also runs with a concurrent
encoder and a barrier between dependent groups. The first run of each is
checked row by row against the CPU reference.

Usage: metal_gemv_bench [--iterations N] [--expert-pool-gb G] [--rows 1|2|4]

  --iterations N      timed steps per workload and variant (optional, default 8)
  --expert-pool-gb G  size of the MXFP4 expert pool in GB (optional, default 4)
  --rows R            only this rows-per-simdgroup variant (optional, default all)
  -h, --help          this text

Example: cargo run --release -p loadngo-metal-compute --bin metal_gemv_bench -- --iterations 12
";

    const HIDDEN: usize = 2304;
    const ATTN: usize = 4096;
    const MOE: usize = 1024;
    const DENSE: usize = 9216;
    const VOCAB: usize = 163_840;
    const LAYERS: usize = 27;
    const EXPERTS_PER_TOKEN: usize = 8;
    /// Largest `cols` of any product: the x buffer holds this many floats.
    const MAX_COLS: usize = DENSE;

    // Buffer positions in every batch.
    const TRUNK: usize = 0;
    const LM_HEAD: usize = 1;
    const ELEMENTS: usize = 2;
    const SCALES: usize = 3;
    const X: usize = 4;
    const Y: usize = 5;

    struct Options {
        iterations: usize,
        pool_bytes: usize,
        rows: Vec<Rows>,
    }

    fn parse() -> Result<Option<Options>, String> {
        let mut options = Options {
            iterations: 8,
            pool_bytes: 4 << 30,
            rows: Rows::ALL.to_vec(),
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let mut value = |name: &str| {
                args.next()
                    .ok_or_else(|| format!("{name} needs a value; pass --help for usage"))
            };
            match arg.as_str() {
                "-h" | "--help" => {
                    print!("{HELP}");
                    return Ok(None);
                }
                "--iterations" => {
                    options.iterations = value("--iterations")?
                        .parse()
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or("--iterations takes a positive whole number")?;
                }
                "--expert-pool-gb" => {
                    let gb: f64 = value("--expert-pool-gb")?
                        .parse()
                        .ok()
                        .filter(|&g| g > 0.0)
                        .ok_or("--expert-pool-gb takes a positive number")?;
                    options.pool_bytes = (gb * f64::from(1u32 << 30)) as usize;
                }
                "--rows" => {
                    let rows = match value("--rows")?.as_str() {
                        "1" => Rows::One,
                        "2" => Rows::Two,
                        "4" => Rows::Four,
                        _ => return Err("--rows takes 1, 2 or 4".into()),
                    };
                    options.rows = vec![rows];
                }
                other => return Err(format!("unknown argument {other}; pass --help for usage")),
            }
        }
        Ok(Some(options))
    }

    /// One product `y = W x`.
    #[derive(Clone, Copy)]
    enum Op {
        Bf16 {
            w: Slice,
            rows: usize,
            cols: usize,
        },
        Mxfp4 {
            elements: Slice,
            scales: Slice,
            rows: usize,
            cols: usize,
        },
    }

    impl Op {
        fn rows(&self) -> usize {
            match *self {
                Op::Bf16 { rows, .. } | Op::Mxfp4 { rows, .. } => rows,
            }
        }

        fn weight_bytes(&self) -> usize {
            match self {
                Op::Bf16 { w, .. } => w.len,
                Op::Mxfp4 {
                    elements, scales, ..
                } => elements.len + scales.len,
            }
        }
    }

    /// Products with no dependency on each other; a workload is a sequence of these.
    type Group = Vec<Op>;

    struct Layout {
        trunk_layers: Vec<Vec<Group>>,
        lm_head: Op,
        expert_count: usize,
        trunk_bytes: usize,
        pool_elements: usize,
        pool_scales: usize,
    }

    fn mxfp4_sizes(rows: usize, cols: usize) -> (usize, usize) {
        (rows * cols.div_ceil(2), rows * cols.div_ceil(32))
    }

    /// Per expert: gate and up (MOE x HIDDEN), down (HIDDEN x MOE).
    fn expert_sizes() -> (usize, usize) {
        let (ge, gs) = mxfp4_sizes(MOE, HIDDEN);
        let (de, ds) = mxfp4_sizes(HIDDEN, MOE);
        (2 * ge + de, 2 * gs + ds)
    }

    fn layout(pool_bytes: usize) -> Layout {
        let mut offset = 0;
        let mut bf16 = |rows: usize, cols: usize| {
            let w = Slice::new(TRUNK, offset, rows * cols * 2);
            offset += w.len;
            Op::Bf16 { w, rows, cols }
        };
        let mut trunk_layers = Vec::with_capacity(LAYERS);
        for layer in 0..LAYERS {
            let attention = vec![
                bf16(ATTN, HIDDEN),
                bf16(ATTN, HIDDEN),
                bf16(ATTN, HIDDEN),
                bf16(HIDDEN, ATTN),
            ];
            // Layer 0 has a dense MLP; the rest a shared expert beside the routed ones.
            let inner = if layer == 0 { DENSE } else { MOE };
            let gate_up = vec![bf16(inner, HIDDEN), bf16(inner, HIDDEN)];
            let down = vec![bf16(HIDDEN, inner)];
            trunk_layers.push(vec![attention, gate_up, down]);
        }
        let lm_head = Op::Bf16 {
            w: Slice::new(LM_HEAD, 0, VOCAB * HIDDEN * 2),
            rows: VOCAB,
            cols: HIDDEN,
        };
        let (expert_elements, expert_scales) = expert_sizes();
        let expert_count =
            (pool_bytes / (expert_elements + expert_scales)).max((LAYERS - 1) * EXPERTS_PER_TOKEN);
        Layout {
            trunk_layers,
            lm_head,
            expert_count,
            trunk_bytes: offset,
            pool_elements: expert_count * expert_elements,
            pool_scales: expert_count * expert_scales,
        }
    }

    /// Expert `id`'s gate, up and down products.
    fn expert_ops(id: usize) -> [Op; 3] {
        let (expert_elements, expert_scales) = expert_sizes();
        let (ge, gs) = mxfp4_sizes(MOE, HIDDEN);
        let (de, ds) = mxfp4_sizes(HIDDEN, MOE);
        let (e0, s0) = (id * expert_elements, id * expert_scales);
        let op = |e: usize, el: usize, s: usize, sl: usize, rows, cols| Op::Mxfp4 {
            elements: Slice::new(ELEMENTS, e, el),
            scales: Slice::new(SCALES, s, sl),
            rows,
            cols,
        };
        [
            op(e0, ge, s0, gs, MOE, HIDDEN),
            op(e0 + ge, ge, s0 + gs, gs, MOE, HIDDEN),
            op(e0 + 2 * ge, de, s0 + 2 * gs, ds, HIDDEN, MOE),
        ]
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Workload {
        LmHead,
        Trunk,
        Experts,
        Token,
    }

    impl Workload {
        fn name(self) -> &'static str {
            match self {
                Workload::LmHead => "lm-head",
                Workload::Trunk => "trunk",
                Workload::Experts => "experts",
                Workload::Token => "token",
            }
        }
    }

    /// The groups of one step of `workload`; `step` picks which experts it reads.
    fn groups(layout: &Layout, workload: Workload, step: usize) -> Vec<Group> {
        let trunk = matches!(workload, Workload::Trunk | Workload::Token);
        let experts = matches!(workload, Workload::Experts | Workload::Token);
        let mut out = Vec::new();
        if workload == Workload::LmHead {
            out.push(vec![layout.lm_head]);
            return out;
        }
        let per_step = (LAYERS - 1) * EXPERTS_PER_TOKEN;
        for (layer, trunk_groups) in layout.trunk_layers.iter().enumerate() {
            let routed: Vec<[Op; 3]> = if experts && layer > 0 {
                (0..EXPERTS_PER_TOKEN)
                    .map(|k| {
                        let n = step * per_step + (layer - 1) * EXPERTS_PER_TOKEN + k;
                        expert_ops(n % layout.expert_count)
                    })
                    .collect()
            } else {
                Vec::new()
            };
            if trunk {
                out.push(trunk_groups[0].clone());
            }
            let mut gate_up: Group = if trunk {
                trunk_groups[1].clone()
            } else {
                vec![]
            };
            gate_up.extend(routed.iter().flat_map(|e| [e[0], e[1]]));
            let mut down: Group = if trunk {
                trunk_groups[2].clone()
            } else {
                vec![]
            };
            down.extend(routed.iter().map(|e| e[2]));
            for group in [gate_up, down] {
                if !group.is_empty() {
                    out.push(group);
                }
            }
        }
        if trunk {
            out.push(vec![layout.lm_head]);
        }
        out
    }

    /// Where each op of `groups` writes in the y buffer, in order.
    fn y_offsets(groups: &[Group]) -> (Vec<usize>, usize) {
        let mut offsets = Vec::new();
        let mut at = 0;
        for op in groups.iter().flatten() {
            offsets.push(at);
            at += op.rows() * 4;
        }
        (offsets, at)
    }

    fn encode(
        batch: &mut Batch<'_>,
        groups: &[Group],
        offsets: &[usize],
        concurrent: bool,
    ) -> Result<(), String> {
        let mut index = 0;
        for (g, group) in groups.iter().enumerate() {
            if concurrent && g > 0 {
                batch.barrier();
            }
            for op in group {
                let y = Slice::new(Y, offsets[index], op.rows() * 4);
                index += 1;
                match *op {
                    Op::Bf16 { w, rows, cols } => {
                        batch.gemv_bf16(w, Slice::new(X, 0, cols * 4), y, rows, cols)
                    }
                    Op::Mxfp4 {
                        elements,
                        scales,
                        rows,
                        cols,
                    } => batch.gemv_mxfp4(
                        elements,
                        scales,
                        Slice::new(X, 0, cols * 4),
                        y,
                        rows,
                        cols,
                    ),
                }
                .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// Commits `batch` and runs the proactor until its completion job has run.
    fn run(proactor: &Proactor<PlatformPort>, batch: Batch<'_>) -> Result<Completed, String> {
        let slot: Arc<Mutex<Option<Completed>>> = Arc::default();
        let filled = Arc::clone(&slot);
        batch.commit(&proactor.handle(), move |done| {
            *filled.lock().expect("result slot") = Some(done);
        });
        loop {
            proactor.run_once().map_err(|e| e.to_string())?;
            if let Some(done) = slot.lock().expect("result slot").take() {
                return Ok(done);
            }
        }
    }

    /// Deterministic pseudo-random fill (xorshift64*), fast enough for gigabytes.
    struct Fill(u64);

    impl Fill {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        /// bfloat16 values of uniform magnitude below 1 (no infinities or NaNs).
        fn bf16(&mut self, out: &mut [u8]) {
            let (chunks, _) = out.as_chunks_mut::<8>();
            for chunk in chunks {
                let r = self.next();
                for (i, pair) in chunk.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    let bits = (r >> (16 * i)) as u16;
                    // sign, exponent 96..=126 (2^-31..2^-1), 7 mantissa bits
                    let exponent = 96 + (bits >> 7 & 0x1f).min(30);
                    let value = (bits & 0x8000) | (exponent << 7) | (bits & 0x7f);
                    pair.copy_from_slice(&value.to_le_bytes());
                }
            }
        }

        fn bytes(&mut self, out: &mut [u8]) {
            for chunk in out.chunks_mut(8) {
                let r = self.next().to_le_bytes();
                chunk.copy_from_slice(&r[..chunk.len()]);
            }
        }

        /// E8M0 scales between 2^-8 and 2^3.
        fn scales(&mut self, out: &mut [u8]) {
            for chunk in out.chunks_mut(8) {
                let r = self.next().to_le_bytes();
                for (o, b) in chunk.iter_mut().zip(r) {
                    *o = 119 + b % 12;
                }
            }
        }
    }

    /// Decodes one weight row into `f32`s.
    type RowValues<'a> = Box<dyn Fn(usize, &mut [f32]) + 'a>;

    /// Checks every row (every 7th of very tall matrices) of the ops against the CPU
    /// reference. The bound is the worst-case float32 summation error for the row.
    fn verify(buffers: &[Buffer], groups: &[Group], offsets: &[usize]) -> Result<usize, String> {
        let x = &buffers[X].as_f32()[..MAX_COLS];
        let y = buffers[Y].as_f32();
        let mut checked = 0;
        for (op, &offset) in groups.iter().flatten().zip(offsets) {
            let (rows, cols) = match *op {
                Op::Bf16 { rows, cols, .. } | Op::Mxfp4 { rows, cols, .. } => (rows, cols),
            };
            let got = &y[offset / 4..offset / 4 + rows];
            let x = &x[..cols];
            let mut want = vec![0.0_f32; rows];
            let mut row = vec![0.0_f32; cols];
            let step = if rows > 16_384 { 7 } else { 1 };
            let row_values: RowValues<'_> = match *op {
                Op::Bf16 { w, .. } => {
                    let bytes = &buffers[w.buffer].as_bytes()[w.offset..w.offset + w.len];
                    let m = HalfMatrix::new(bytes, rows, cols, HalfFormat::Bf16)
                        .map_err(|e| e.to_string())?;
                    if step == 1 {
                        m.mul_vec(&mut want, x);
                    }
                    Box::new(move |r, out| m.row_into(r, out))
                }
                Op::Mxfp4 {
                    elements, scales, ..
                } => {
                    let e = &buffers[elements.buffer].as_bytes()
                        [elements.offset..elements.offset + elements.len];
                    let s = &buffers[scales.buffer].as_bytes()
                        [scales.offset..scales.offset + scales.len];
                    let m = Mxfp4Matrix::new(e, s, rows, cols).map_err(|e| e.to_string())?;
                    m.mul_vec(&mut want, x);
                    Box::new(move |r, out| m.dequantize_row(r, out))
                }
            };
            for r in (0..rows).step_by(step) {
                row_values(r, &mut row);
                let (mut exact, mut magnitude) = (0.0_f64, 0.0_f64);
                for (&w, &xc) in row.iter().zip(x) {
                    let p = f64::from(w) * f64::from(xc);
                    exact += p;
                    magnitude += p.abs();
                }
                if step == 1 && (f64::from(want[r]) - exact).abs() > 1e-6 * magnitude {
                    return Err(format!(
                        "CPU reference disagrees with itself at row {r}: {} vs {exact}",
                        want[r]
                    ));
                }
                let bound = cols as f64 * f64::from(f32::EPSILON) * magnitude + 1e-30;
                let error = (f64::from(got[r]) - exact).abs();
                if error.is_nan() || error > bound {
                    return Err(format!(
                        "{rows}x{cols} row {r}: GPU {} vs reference {exact} (bound {bound:e})",
                        got[r]
                    ));
                }
                checked += 1;
            }
        }
        Ok(checked)
    }

    fn median(values: &mut [f64]) -> f64 {
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    }

    pub fn main() -> Result<(), String> {
        let Some(options) = parse()? else {
            return Ok(());
        };
        let mut gpu = Gpu::new().map_err(|e| e.to_string())?;
        let proactor = new_platform_proactor().map_err(|e| e.to_string())?;
        let layout = layout(options.pool_bytes);
        let gb = |bytes: usize| bytes as f64 / 1e9;
        println!(
            "{} (recommended working set {:.1} GB)",
            gpu.name(),
            gpu.recommended_working_set() as f64 / 1e9
        );
        println!(
            "weights: trunk {:.2} GB, LM head {:.2} GB, expert pool {:.2} GB ({} experts)",
            gb(layout.trunk_bytes),
            gb(layout.lm_head.weight_bytes()),
            gb(layout.pool_elements + layout.pool_scales),
            layout.expert_count
        );

        let started = std::time::Instant::now();
        let mut fill = Fill(0x5eed_1234_abcd_ef01);
        let mut buffers = Vec::new();
        for (len, kind) in [
            (layout.trunk_bytes, 0),
            (layout.lm_head.weight_bytes(), 0),
            (layout.pool_elements, 1),
            (layout.pool_scales, 2),
            (MAX_COLS * 4, 3),
        ] {
            let mut buffer = gpu.buffer(len).map_err(|e| e.to_string())?;
            match kind {
                0 => fill.bf16(buffer.as_bytes_mut()),
                1 => fill.bytes(buffer.as_bytes_mut()),
                2 => fill.scales(buffer.as_bytes_mut()),
                _ => {
                    for v in buffer.as_f32_mut() {
                        *v = (fill.next() >> 40) as f32 / (1u64 << 23) as f32 - 1.0;
                    }
                }
            }
            buffers.push(buffer);
        }
        let (_, largest_y) = y_offsets(&groups(&layout, Workload::Token, 0));
        buffers.push(gpu.buffer(largest_y).map_err(|e| e.to_string())?);
        println!("filled in {:.1} s", started.elapsed().as_secs_f64());
        println!();
        println!(
            "{:<8} {:>4} {:>10} {:>9} {:>10} {:>9} {:>9} {:>8}",
            "workload",
            "rows",
            "encoder",
            "dispatch",
            "GB/step",
            "ms (med)",
            "GB/s med",
            "GB/s max"
        );

        let plan = [
            (Workload::LmHead, false),
            (Workload::Trunk, false),
            (Workload::Experts, false),
            (Workload::Experts, true),
            (Workload::Token, false),
            (Workload::Token, true),
        ];
        let mut checked = 0;
        for &rows in &options.rows {
            gpu.set_rows(rows);
            for &(workload, concurrent) in &plan {
                let mut seconds = Vec::with_capacity(options.iterations);
                let mut bytes = 0;
                let mut dispatches = 0;
                // One untimed warm-up step, which is also the one verified.
                for step in 0..=options.iterations {
                    let groups = groups(&layout, workload, step);
                    let (offsets, _) = y_offsets(&groups);
                    let mut batch = gpu
                        .batch(
                            std::mem::take(&mut buffers),
                            if concurrent {
                                Dispatch::Concurrent
                            } else {
                                Dispatch::Serial
                            },
                        )
                        .map_err(|e| e.to_string())?;
                    encode(&mut batch, &groups, &offsets, concurrent)?;
                    dispatches = batch.dispatches();
                    let done = run(&proactor, batch)?;
                    buffers = done.buffers;
                    let time: Duration = done.gpu_time.map_err(|e| e.to_string())?;
                    if step == 0 {
                        checked += verify(&buffers, &groups, &offsets)?;
                    } else {
                        seconds.push(time.as_secs_f64());
                        bytes = groups.iter().flatten().map(Op::weight_bytes).sum();
                    }
                }
                let med = median(&mut seconds);
                let best = seconds.first().copied().unwrap_or(med);
                println!(
                    "{:<8} {:>4} {:>10} {:>9} {:>10.3} {:>9.2} {:>9.1} {:>8.1}",
                    workload.name(),
                    rows.count(),
                    if concurrent { "concurrent" } else { "serial" },
                    dispatches,
                    gb(bytes),
                    med * 1e3,
                    gb(bytes) / med,
                    gb(bytes) / best,
                );
            }
        }
        println!();
        println!("verified {checked} rows against the CPU reference");
        Ok(())
    }
}
