// Matrix-vector kernels for loadngo-metal-compute. Compiled from source at load time.
//
// y = W x, W row-major, x and y float32, accumulation in float32. Each simdgroup (32
// threads) owns ROWS consecutive rows; its lanes stride across a row in 16-byte loads,
// so one simdgroup step reads 512 contiguous bytes of each row, and every x chunk it
// loads is reused for all ROWS rows. Partial sums meet in simd_sum.
//
// Formats, from the published specifications:
// - bfloat16: the upper half of an IEEE binary32, so widening is a 16-bit shift.
// - MXFP4 (OCP MX v1.0): blocks of 32 E2M1 elements, two per byte, low nibble first,
//   and one E8M0 scale byte per block (2^(s-127); 0xFF is NaN).

#include <metal_stdlib>
using namespace metal;

struct GemvArgs {
    uint rows;
    uint cols;
};

constant constexpr uint SIMD = 32;

inline float bf16_lo(uint pair) { return as_type<float>(pair << 16); }
inline float bf16_hi(uint pair) { return as_type<float>(pair & 0xffff0000u); }

inline float dot_bf16x8(uint4 w, float4 x0, float4 x1) {
    return bf16_lo(w.x) * x0.x + bf16_hi(w.x) * x0.y
         + bf16_lo(w.y) * x0.z + bf16_hi(w.y) * x0.w
         + bf16_lo(w.z) * x1.x + bf16_hi(w.z) * x1.y
         + bf16_lo(w.w) * x1.z + bf16_hi(w.w) * x1.w;
}

template <uint ROWS>
kernel void gemv_bf16(
    device const uchar *w [[buffer(0)]],
    device const float *x [[buffer(1)]],
    device float *y [[buffer(2)]],
    constant GemvArgs &a [[buffer(3)]],
    uint tg [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint sgs [[simdgroups_per_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{
    const uint first = (tg * sgs + sg) * ROWS;
    if (first >= a.rows) {
        return;
    }
    const uint count = min(ROWS, a.rows - first);
    float acc[ROWS];
    for (uint r = 0; r < ROWS; ++r) {
        acc[r] = 0.0f;
    }

    if ((a.cols & 7) == 0) {
        // Rows start on 16-byte boundaries (the host checks the buffer offset).
        const uint chunks = a.cols / 8;
        device const uint4 *rows4 = (device const uint4 *)w;
        device const float4 *x4 = (device const float4 *)x;
        for (uint c = lane; c < chunks; c += SIMD) {
            const float4 x0 = x4[2 * c];
            const float4 x1 = x4[2 * c + 1];
            for (uint r = 0; r < ROWS; ++r) {
                if (r < count) {
                    acc[r] += dot_bf16x8(rows4[(ulong)(first + r) * chunks + c], x0, x1);
                }
            }
        }
    } else {
        device const ushort *w16 = (device const ushort *)w;
        for (uint c = lane; c < a.cols; c += SIMD) {
            const float xc = x[c];
            for (uint r = 0; r < ROWS; ++r) {
                if (r < count) {
                    const uint bits = w16[(ulong)(first + r) * a.cols + c];
                    acc[r] += as_type<float>(bits << 16) * xc;
                }
            }
        }
    }

    for (uint r = 0; r < count; ++r) {
        const float sum = simd_sum(acc[r]);
        if (lane == 0) {
            y[first + r] = sum;
        }
    }
}

// E2M1 code -> value. Codes 0..7: 0, 0.5, 1, 1.5, 2, 3, 4, 6; bit 3 is the sign.
constant float E2M1[16] = {
    0.0f, 0.5f, 1.0f, 1.5f, 2.0f, 3.0f, 4.0f, 6.0f,
    -0.0f, -0.5f, -1.0f, -1.5f, -2.0f, -3.0f, -4.0f, -6.0f,
};

inline float e8m0(uint s) {
    if (s == 0xffu) {
        return NAN;
    }
    // 2^-127 is subnormal in binary32: exponent field 0, top mantissa bit set.
    return s == 0 ? as_type<float>(0x00400000u) : as_type<float>(s << 23);
}

// Eight packed elements (one 32-bit word, low nibble first) dotted with eight x values.
inline float dot_e2m1x8(uint word, threadgroup const float *lut, float4 x0, float4 x1) {
    return lut[word & 15] * x0.x + lut[(word >> 4) & 15] * x0.y
         + lut[(word >> 8) & 15] * x0.z + lut[(word >> 12) & 15] * x0.w
         + lut[(word >> 16) & 15] * x1.x + lut[(word >> 20) & 15] * x1.y
         + lut[(word >> 24) & 15] * x1.z + lut[word >> 28] * x1.w;
}

template <uint ROWS>
kernel void gemv_mxfp4(
    device const uchar *elements [[buffer(0)]],
    device const uchar *scales [[buffer(1)]],
    device const float *x [[buffer(2)]],
    device float *y [[buffer(3)]],
    constant GemvArgs &a [[buffer(4)]],
    uint tg [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint sgs [[simdgroups_per_threadgroup]],
    uint lane [[thread_index_in_simdgroup]],
    uint tid [[thread_index_in_threadgroup]])
{
    threadgroup float lut[16];
    if (tid < 16) {
        lut[tid] = E2M1[tid];
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);

    const uint first = (tg * sgs + sg) * ROWS;
    if (first >= a.rows) {
        return;
    }
    const uint count = min(ROWS, a.rows - first);
    const uint row_bytes = (a.cols + 1) / 2;
    const uint blocks = (a.cols + 31) / 32;
    float acc[ROWS];
    for (uint r = 0; r < ROWS; ++r) {
        acc[r] = 0.0f;
    }

    if ((a.cols & 31) == 0) {
        // Whole blocks: 16 element bytes per block, rows on 16-byte boundaries.
        device const uint4 *e4 = (device const uint4 *)elements;
        device const float4 *x4 = (device const float4 *)x;
        for (uint b = lane; b < blocks; b += SIMD) {
            float4 xs[8];
            for (uint i = 0; i < 8; ++i) {
                xs[i] = x4[8 * b + i];
            }
            for (uint r = 0; r < ROWS; ++r) {
                if (r < count) {
                    const ulong row = first + r;
                    const uint4 q = e4[row * blocks + b];
                    const float block = dot_e2m1x8(q.x, lut, xs[0], xs[1])
                                      + dot_e2m1x8(q.y, lut, xs[2], xs[3])
                                      + dot_e2m1x8(q.z, lut, xs[4], xs[5])
                                      + dot_e2m1x8(q.w, lut, xs[6], xs[7]);
                    acc[r] += block * e8m0(scales[row * blocks + b]);
                }
            }
        }
    } else {
        for (uint b = lane; b < blocks; b += SIMD) {
            const uint start = b * 32;
            const uint end = min(start + 32, a.cols);
            for (uint r = 0; r < ROWS; ++r) {
                if (r < count) {
                    const ulong row = first + r;
                    device const uchar *packed = elements + row * row_bytes;
                    float block = 0.0f;
                    for (uint c = start; c < end; ++c) {
                        const uint byte = packed[c / 2];
                        const uint code = (c & 1) ? (byte >> 4) : (byte & 15);
                        block += lut[code] * x[c];
                    }
                    acc[r] += block * e8m0(scales[row * blocks + b]);
                }
            }
        }
    }

    for (uint r = 0; r < count; ++r) {
        const float sum = simd_sum(acc[r]);
        if (lane == 0) {
            y[first + r] = sum;
        }
    }
}

// Several positions at once: y[p] = W x[p] for n positions, reading each weight row once
// per N positions instead of once per position. x rows are x_stride floats apart
// (a multiple of 4, rows 16-byte aligned), y rows y_stride floats apart.
struct GemmArgs {
    uint rows;
    uint cols;
    uint n;
    uint x_stride;
    uint y_stride;
};

constant constexpr uint GEMM_ROWS = 2;
constant constexpr uint GEMM_N = 8;

kernel void gemm_bf16(
    device const uchar *w [[buffer(0)]],
    device const float *x [[buffer(1)]],
    device float *y [[buffer(2)]],
    constant GemmArgs &a [[buffer(3)]],
    uint tg [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint sgs [[simdgroups_per_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{
    const uint first = (tg * sgs + sg) * GEMM_ROWS;
    if (first >= a.rows) {
        return;
    }
    const uint count = min(GEMM_ROWS, a.rows - first);
    for (uint p0 = 0; p0 < a.n; p0 += GEMM_N) {
        const uint np = min(GEMM_N, a.n - p0);
        float acc[GEMM_ROWS][GEMM_N];
        for (uint r = 0; r < GEMM_ROWS; ++r) {
            for (uint p = 0; p < GEMM_N; ++p) {
                acc[r][p] = 0.0f;
            }
        }
        if ((a.cols & 7) == 0) {
            const uint chunks = a.cols / 8;
            device const uint4 *rows4 = (device const uint4 *)w;
            for (uint c = lane; c < chunks; c += SIMD) {
                uint4 wv[GEMM_ROWS];
                for (uint r = 0; r < GEMM_ROWS; ++r) {
                    wv[r] = r < count ? rows4[(ulong)(first + r) * chunks + c] : uint4(0);
                }
                for (uint p = 0; p < GEMM_N; ++p) {
                    if (p < np) {
                        device const float4 *xp =
                            (device const float4 *)(x + (ulong)(p0 + p) * a.x_stride);
                        const float4 x0 = xp[2 * c];
                        const float4 x1 = xp[2 * c + 1];
                        for (uint r = 0; r < GEMM_ROWS; ++r) {
                            acc[r][p] += dot_bf16x8(wv[r], x0, x1);
                        }
                    }
                }
            }
        } else {
            device const ushort *w16 = (device const ushort *)w;
            for (uint c = lane; c < a.cols; c += SIMD) {
                float wc[GEMM_ROWS];
                for (uint r = 0; r < GEMM_ROWS; ++r) {
                    wc[r] = r < count
                        ? as_type<float>(uint(w16[(ulong)(first + r) * a.cols + c]) << 16)
                        : 0.0f;
                }
                for (uint p = 0; p < GEMM_N; ++p) {
                    if (p < np) {
                        const float xc = x[(ulong)(p0 + p) * a.x_stride + c];
                        for (uint r = 0; r < GEMM_ROWS; ++r) {
                            acc[r][p] += wc[r] * xc;
                        }
                    }
                }
            }
        }
        for (uint r = 0; r < count; ++r) {
            for (uint p = 0; p < np; ++p) {
                const float sum = simd_sum(acc[r][p]);
                if (lane == 0) {
                    y[(ulong)(p0 + p) * a.y_stride + first + r] = sum;
                }
            }
        }
    }
}

kernel void gemm_mxfp4(
    device const uchar *elements [[buffer(0)]],
    device const uchar *scales [[buffer(1)]],
    device const float *x [[buffer(2)]],
    device float *y [[buffer(3)]],
    constant GemmArgs &a [[buffer(4)]],
    uint tg [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint sgs [[simdgroups_per_threadgroup]],
    uint lane [[thread_index_in_simdgroup]],
    uint tid [[thread_index_in_threadgroup]])
{
    threadgroup float lut[16];
    if (tid < 16) {
        lut[tid] = E2M1[tid];
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);

    const uint first = (tg * sgs + sg) * GEMM_ROWS;
    if (first >= a.rows) {
        return;
    }
    const uint count = min(GEMM_ROWS, a.rows - first);
    const uint row_bytes = (a.cols + 1) / 2;
    const uint blocks = (a.cols + 31) / 32;
    for (uint p0 = 0; p0 < a.n; p0 += GEMM_N) {
        const uint np = min(GEMM_N, a.n - p0);
        float acc[GEMM_ROWS][GEMM_N];
        for (uint r = 0; r < GEMM_ROWS; ++r) {
            for (uint p = 0; p < GEMM_N; ++p) {
                acc[r][p] = 0.0f;
            }
        }
        if ((a.cols & 31) == 0) {
            // Whole blocks: each row's block is decoded once into registers, then dotted
            // with every position's x.
            device const uint4 *e4 = (device const uint4 *)elements;
            for (uint b = lane; b < blocks; b += SIMD) {
                for (uint r = 0; r < GEMM_ROWS; ++r) {
                    if (r < count) {
                        const ulong row = first + r;
                        const uint4 q = e4[row * blocks + b];
                        const float scale = e8m0(scales[row * blocks + b]);
                        float4 wv[8];
                        for (uint i = 0; i < 4; ++i) {
                            const uint word = q[i];
                            wv[2 * i] = float4(lut[word & 15], lut[(word >> 4) & 15],
                                               lut[(word >> 8) & 15], lut[(word >> 12) & 15]);
                            wv[2 * i + 1] = float4(lut[(word >> 16) & 15], lut[(word >> 20) & 15],
                                                   lut[(word >> 24) & 15], lut[word >> 28]);
                        }
                        for (uint p = 0; p < GEMM_N; ++p) {
                            if (p < np) {
                                device const float4 *xp = (device const float4 *)(
                                    x + (ulong)(p0 + p) * a.x_stride) + 8 * b;
                                float block = 0.0f;
                                for (uint i = 0; i < 8; ++i) {
                                    block += dot(wv[i], xp[i]);
                                }
                                acc[r][p] += block * scale;
                            }
                        }
                    }
                }
            }
        } else {
            for (uint b = lane; b < blocks; b += SIMD) {
                const uint start = b * 32;
                const uint end = min(start + 32, a.cols);
                for (uint r = 0; r < count; ++r) {
                    const ulong row = first + r;
                    device const uchar *packed = elements + row * row_bytes;
                    const float scale = e8m0(scales[row * blocks + b]);
                    for (uint p = 0; p < GEMM_N; ++p) {
                        if (p < np) {
                            device const float *xp = x + (ulong)(p0 + p) * a.x_stride;
                            float block = 0.0f;
                            for (uint c = start; c < end; ++c) {
                                const uint byte = packed[c / 2];
                                block += lut[(c & 1) ? (byte >> 4) : (byte & 15)] * xp[c];
                            }
                            acc[r][p] += block * scale;
                        }
                    }
                }
            }
        }
        for (uint r = 0; r < count; ++r) {
            for (uint p = 0; p < np; ++p) {
                const float sum = simd_sum(acc[r][p]);
                if (lane == 0) {
                    y[(ulong)(p0 + p) * a.y_stride + first + r] = sum;
                }
            }
        }
    }
}

#define GEMV_VARIANTS(ROWS) \
    template [[host_name("gemv_bf16_r" #ROWS)]] kernel void gemv_bf16<ROWS>( \
        device const uchar *, device const float *, device float *, constant GemvArgs &, \
        uint, uint, uint, uint); \
    template [[host_name("gemv_mxfp4_r" #ROWS)]] kernel void gemv_mxfp4<ROWS>( \
        device const uchar *, device const uchar *, device const float *, device float *, \
        constant GemvArgs &, uint, uint, uint, uint, uint);

GEMV_VARIANTS(1)
GEMV_VARIANTS(2)
GEMV_VARIANTS(4)

// Causal multi-head attention whose keys have two parts: one per head, stored with that
// head's values, and one shared by every head (as in multi-head latent attention with a
// decoupled rotary key). For new position i (absolute position cached + i) and head h:
//   score(s) = scale * (q[i][h][:qa] . kv[s][h][:qa] + q[i][h][qa:] . shared[s])
//   out[i][h] = sum over s <= cached + i of softmax(score)(s) * kv[s][h][qa:qa + dv]
// Layouts, float32: q [t][heads][qa + qb], kv [positions][heads][qa + dv],
// shared [positions][qb], out [t][heads][dv]. Needs qa + qb <= 256 and dv <= 128.
//
// One threadgroup per (block of 8 new positions, head); simdgroup g takes position
// block * 8 + g and walks the cache in order, so the block's simdgroups read each cached
// row at about the same time. Lanes split the dot product and the value dimensions; the
// softmax is computed online (running maximum, sum and weighted values), in float32.
struct AttentionArgs {
    uint t;
    uint cached;
    uint heads;
    uint qa;
    uint qb;
    uint dv;
    float scale;
};

constant constexpr uint ATTENTION_BLOCK = 8;

kernel void attention_split_key(
    device const float *q [[buffer(0)]],
    device const float *kv [[buffer(1)]],
    device const float *shared_key [[buffer(2)]],
    device float *out [[buffer(3)]],
    constant AttentionArgs &a [[buffer(4)]],
    uint group [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{
    const uint head = group % a.heads;
    const uint step = (group / a.heads) * ATTENTION_BLOCK + sg;
    if (sg >= ATTENTION_BLOCK || step >= a.t) {
        return;
    }
    const uint dq = a.qa + a.qb;
    const uint row = a.qa + a.dv;
    device const float *qt = q + ((ulong)step * a.heads + head) * dq;
    float qv[8];
    for (uint i = 0; i < 8; ++i) {
        const uint d = lane + SIMD * i;
        qv[i] = d < dq ? qt[d] : 0.0f;
    }
    float m = -INFINITY;
    float l = 0.0f;
    float acc[4] = {0.0f, 0.0f, 0.0f, 0.0f};
    const uint last = a.cached + step;
    for (uint s = 0; s <= last; ++s) {
        device const float *k = kv + ((ulong)s * a.heads + head) * row;
        device const float *r = shared_key + (ulong)s * a.qb;
        float dot = 0.0f;
        for (uint i = 0; i < 8; ++i) {
            const uint d = lane + SIMD * i;
            if (d < a.qa) {
                dot += qv[i] * k[d];
            } else if (d < dq) {
                dot += qv[i] * r[d - a.qa];
            }
        }
        const float score = simd_sum(dot) * a.scale;
        const float next = max(m, score);
        const float keep = exp(m - next);
        const float p = exp(score - next);
        l = l * keep + p;
        for (uint j = 0; j < 4; ++j) {
            const uint d = lane + SIMD * j;
            if (d < a.dv) {
                acc[j] = acc[j] * keep + p * k[a.qa + d];
            }
        }
        m = next;
    }
    device float *o = out + ((ulong)step * a.heads + head) * a.dv;
    for (uint j = 0; j < 4; ++j) {
        const uint d = lane + SIMD * j;
        if (d < a.dv) {
            o[d] = acc[j] / l;
        }
    }
}

// The delta-rule recurrence with a per-key-channel decay (Kimi Delta Attention; gated
// DeltaNet when the decay is per head). Per head, with state S [dk][dv], for each step:
//   S = diag(alpha) S;  u = S^T k;  S += k (beta (v - u))^T;  out = S^T q
// Layouts, float32: q, k, alpha [t][heads][dk]; v and out [t][heads][dv]; beta
// [t][heads]; state [heads][dk][dv], read at the start and written back at the end.
// Needs dk even, dk <= 128 and dv <= 128.
//
// One threadgroup of 256 threads per head, steps in order. Thread 2j + h holds column j
// of S, rows h * dk / 2 onwards, in registers; the two halves meet in a lane shuffle.
// The step's k, q and alpha are staged in threadgroup memory.
struct RecurrenceArgs {
    uint t;
    uint heads;
    uint dk;
    uint dv;
};

constant constexpr uint RECURRENCE_HALF = 64;

kernel void delta_rule_recurrence(
    device const float *q [[buffer(0)]],
    device const float *k [[buffer(1)]],
    device const float *v [[buffer(2)]],
    device const float *alpha [[buffer(3)]],
    device const float *beta [[buffer(4)]],
    device float *state [[buffer(5)]],
    device float *out [[buffer(6)]],
    constant RecurrenceArgs &a [[buffer(7)]],
    uint head [[threadgroup_position_in_grid]],
    uint tid [[thread_index_in_threadgroup]],
    uint threads [[threads_per_threadgroup]])
{
    threadgroup float staged[3 * 2 * RECURRENCE_HALF];
    threadgroup float *sk = staged;
    threadgroup float *sq = staged + 2 * RECURRENCE_HALF;
    threadgroup float *sa = staged + 4 * RECURRENCE_HALF;
    const uint j = tid >> 1;
    const uint half_index = tid & 1;
    const bool column = j < a.dv;
    const uint rows = a.dk / 2;
    const uint r0 = half_index * rows;
    device float *sh = state + (ulong)head * a.dk * a.dv;
    float s[RECURRENCE_HALF];
    for (uint r = 0; r < RECURRENCE_HALF; ++r) {
        s[r] = (column && r < rows) ? sh[(ulong)(r0 + r) * a.dv + j] : 0.0f;
    }
    for (uint step = 0; step < a.t; ++step) {
        const ulong at = (ulong)step * a.heads + head;
        threadgroup_barrier(mem_flags::mem_threadgroup);
        for (uint i = tid; i < a.dk; i += threads) {
            sk[i] = k[at * a.dk + i];
            sq[i] = q[at * a.dk + i];
            sa[i] = alpha[at * a.dk + i];
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        float u = 0.0f;
        for (uint r = 0; r < RECURRENCE_HALF; ++r) {
            if (r < rows) {
                s[r] *= sa[r0 + r];
                u += sk[r0 + r] * s[r];
            }
        }
        u += simd_shuffle_xor(u, 1);
        const float b = beta[at];
        const float delta = (column ? v[at * a.dv + j] : 0.0f) - u;
        float o = 0.0f;
        for (uint r = 0; r < RECURRENCE_HALF; ++r) {
            if (r < rows) {
                s[r] += sk[r0 + r] * b * delta;
                o += sq[r0 + r] * s[r];
            }
        }
        o += simd_shuffle_xor(o, 1);
        if (column && half_index == 0) {
            out[at * a.dv + j] = o;
        }
    }
    for (uint r = 0; r < RECURRENCE_HALF; ++r) {
        if (column && r < rows) {
            sh[(ulong)(r0 + r) * a.dv + j] = s[r];
        }
    }
}

// attention_split_key for a few new positions (decoding): one threadgroup per (new
// position, head), whose 8 simdgroups take every 8th cached position and each keep an
// online softmax; the partial results meet in threadgroup memory at the end.
kernel void attention_split_key_wide(
    device const float *q [[buffer(0)]],
    device const float *kv [[buffer(1)]],
    device const float *shared_key [[buffer(2)]],
    device float *out [[buffer(3)]],
    constant AttentionArgs &a [[buffer(4)]],
    uint group [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{
    threadgroup float part_max[ATTENTION_BLOCK];
    threadgroup float part_sum[ATTENTION_BLOCK];
    threadgroup float part_acc[ATTENTION_BLOCK][128];
    const uint head = group % a.heads;
    const uint step = group / a.heads;
    const uint dq = a.qa + a.qb;
    const uint row = a.qa + a.dv;
    device const float *qt = q + ((ulong)step * a.heads + head) * dq;
    float qv[8];
    for (uint i = 0; i < 8; ++i) {
        const uint d = lane + SIMD * i;
        qv[i] = d < dq ? qt[d] : 0.0f;
    }
    float m = -INFINITY;
    float l = 0.0f;
    float acc[4] = {0.0f, 0.0f, 0.0f, 0.0f};
    const uint last = a.cached + step;
    for (uint s = sg; s <= last; s += ATTENTION_BLOCK) {
        device const float *k = kv + ((ulong)s * a.heads + head) * row;
        device const float *r = shared_key + (ulong)s * a.qb;
        float dot = 0.0f;
        for (uint i = 0; i < 8; ++i) {
            const uint d = lane + SIMD * i;
            if (d < a.qa) {
                dot += qv[i] * k[d];
            } else if (d < dq) {
                dot += qv[i] * r[d - a.qa];
            }
        }
        const float score = simd_sum(dot) * a.scale;
        const float next = max(m, score);
        const float keep = exp(m - next);
        const float p = exp(score - next);
        l = l * keep + p;
        for (uint j = 0; j < 4; ++j) {
            const uint d = lane + SIMD * j;
            if (d < a.dv) {
                acc[j] = acc[j] * keep + p * k[a.qa + d];
            }
        }
        m = next;
    }
    if (sg < ATTENTION_BLOCK) {
        if (lane == 0) {
            part_max[sg] = m;
            part_sum[sg] = l;
        }
        for (uint j = 0; j < 4; ++j) {
            const uint d = lane + SIMD * j;
            if (d < a.dv) {
                part_acc[sg][d] = acc[j];
            }
        }
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);
    if (sg == 0) {
        float total_max = -INFINITY;
        for (uint g = 0; g < ATTENTION_BLOCK; ++g) {
            total_max = max(total_max, part_max[g]);
        }
        float total = 0.0f;
        float w[ATTENTION_BLOCK];
        for (uint g = 0; g < ATTENTION_BLOCK; ++g) {
            // A simdgroup with no positions has max -inf and sum 0: weight 0.
            w[g] = exp(part_max[g] - total_max);
            total += part_sum[g] * w[g];
        }
        device float *o = out + ((ulong)step * a.heads + head) * a.dv;
        for (uint j = 0; j < 4; ++j) {
            const uint d = lane + SIMD * j;
            if (d < a.dv) {
                float v = 0.0f;
                for (uint g = 0; g < ATTENTION_BLOCK; ++g) {
                    v += part_acc[g][d] * w[g];
                }
                o[d] = v / total;
            }
        }
    }
}

// ---- Elementwise and row kernels: the glue between products in one command buffer ----
// All operate on contiguous float32 rows. 256 threads per threadgroup.

// Depthwise causal convolution then SiLU: for row r and channel c,
//   acc = taps[c][K-1] * x[r][c] + sum over h < K-1 of taps[c][h] * input(r - (K-1) + h)
//   out[r][c] = acc * sigmoid(acc)
// where input(i) is x[i][c] for i >= 0 and history[c][K-1 + i] (the previous call's last
// K-1 inputs, oldest first) for i < 0. Needs K <= 8. One thread per (row, channel).
struct ConvArgs {
    uint rows;
    uint channels;
    uint width; // K, the taps per channel
};

kernel void causal_conv_silu(
    device const float *x [[buffer(0)]],
    device const float *taps [[buffer(1)]],
    device const float *history [[buffer(2)]],
    device float *out [[buffer(3)]],
    constant ConvArgs &a [[buffer(4)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= a.rows * a.channels) {
        return;
    }
    const uint r = gid / a.channels;
    const uint c = gid % a.channels;
    const uint hist = a.width - 1;
    device const float *t = taps + (ulong)c * a.width;
    float acc = t[hist] * x[gid];
    for (uint h = 0; h < hist; ++h) {
        const int i = int(r) - int(hist) + int(h);
        const float past = i >= 0 ? x[(ulong)i * a.channels + c]
                                  : history[(ulong)c * hist + uint(int(hist) + i)];
        acc += t[h] * past;
    }
    out[gid] = acc * (1.0f / (1.0f + exp(-acc)));
}

// The history causal_conv_silu reads next time: each channel's last K-1 inputs, from x
// or, when there are fewer rows than that, partly from the old history. One thread per
// channel; run it after the convolution that reads the old history.
kernel void causal_conv_history(
    device const float *x [[buffer(0)]],
    device float *history [[buffer(1)]],
    constant ConvArgs &a [[buffer(2)]],
    uint c [[thread_position_in_grid]])
{
    if (c >= a.channels) {
        return;
    }
    const uint hist = a.width - 1;
    device float *hc = history + (ulong)c * hist;
    float old[8];
    for (uint h = 0; h < hist; ++h) {
        old[h] = hc[h];
    }
    for (uint h = 0; h < hist; ++h) {
        const int i = int(a.rows) - int(hist) + int(h);
        hc[h] = i >= 0 ? x[(ulong)i * a.channels + c] : old[uint(int(a.rows) + int(h))];
    }
}

// Each row of width d, in place: v = (v / sqrt(sum v^2 + eps)) * scale. One simdgroup
// per row.
struct RowArgs {
    uint rows;
    uint d;
    float eps;
    float scale;
};

kernel void l2norm_rows(
    device float *v [[buffer(0)]],
    constant RowArgs &a [[buffer(1)]],
    uint group [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{
    const uint row = group * 8 + sg;
    if (row >= a.rows) {
        return;
    }
    device float *x = v + (ulong)row * a.d;
    float ss = 0.0f;
    for (uint i = lane; i < a.d; i += SIMD) {
        ss += x[i] * x[i];
    }
    const float inv = 1.0f / sqrt(simd_sum(ss) + a.eps);
    for (uint i = lane; i < a.d; i += SIMD) {
        x[i] = x[i] * inv * a.scale;
    }
}

// Each row of width d, in place: v = w * v / sqrt(mean(v^2) + eps) * sigmoid(gate), with
// gate shaped like v and w of width d. One simdgroup per row.
kernel void rmsnorm_gated_rows(
    device float *v [[buffer(0)]],
    device const float *gate [[buffer(1)]],
    device const float *w [[buffer(2)]],
    constant RowArgs &a [[buffer(3)]],
    uint group [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{
    const uint row = group * 8 + sg;
    if (row >= a.rows) {
        return;
    }
    device float *x = v + (ulong)row * a.d;
    device const float *g = gate + (ulong)row * a.d;
    float ss = 0.0f;
    for (uint i = lane; i < a.d; i += SIMD) {
        ss += x[i] * x[i];
    }
    const float inv = 1.0f / sqrt(simd_sum(ss) / float(a.d) + a.eps);
    for (uint i = lane; i < a.d; i += SIMD) {
        x[i] = w[i] * x[i] * inv * (1.0f / (1.0f + exp(-g[i])));
    }
}

// Decay from a log-space rate: for element i of a row of width `width` whose heads are
// `d` wide, alpha[i] = exp(-exp(a_log[head]) * softplus(z[i] + bias[i % width])), with
// softplus(x) = x above 20, else log(1 + exp(x)) (as fla's naive gate).
struct DecayArgs {
    uint n;
    uint width;
    uint d;
};

kernel void softplus_decay(
    device const float *z [[buffer(0)]],
    device const float *a_log [[buffer(1)]],
    device const float *bias [[buffer(2)]],
    device float *alpha [[buffer(3)]],
    constant DecayArgs &a [[buffer(4)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= a.n) {
        return;
    }
    const uint col = gid % a.width;
    const float x = z[gid] + bias[col];
    // log1p(e^x) is e^x to float precision below -15, where 1 + e^x would round to 1.
    const float sp = x > 20.0f ? x : (x < -15.0f ? exp(x) : log(1.0f + exp(x)));
    alpha[gid] = exp(-exp(a_log[col / a.d]) * sp);
}

// v = sigmoid(v), in place, for n elements.
kernel void sigmoid_in_place(
    device float *v [[buffer(0)]],
    constant DecayArgs &a [[buffer(1)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid < a.n) {
        v[gid] = 1.0f / (1.0f + exp(-v[gid]));
    }
}

// g = silu(g) * u in place, silu(x) = x * sigmoid(x), over n elements.
kernel void silu_mul(
    device float *g [[buffer(0)]],
    device const float *u [[buffer(1)]],
    constant DecayArgs &a [[buffer(2)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid < a.n) {
        const float x = g[gid];
        g[gid] = x * (1.0f / (1.0f + exp(-x))) * u[gid];
    }
}

// The first d floats of each of `rows` rows, `stride` floats apart, in place:
// v = w * v / sqrt(mean(v^2) + eps). One simdgroup per row.
struct StridedArgs {
    uint rows;
    uint d;
    uint stride;
    float eps;
};

kernel void rmsnorm_rows(
    device float *v [[buffer(0)]],
    device const float *w [[buffer(1)]],
    constant StridedArgs &a [[buffer(2)]],
    uint group [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{
    const uint row = group * 8 + sg;
    if (row >= a.rows) {
        return;
    }
    device float *x = v + (ulong)row * a.stride;
    float ss = 0.0f;
    for (uint i = lane; i < a.d; i += SIMD) {
        ss += x[i] * x[i];
    }
    const float inv = 1.0f / sqrt(simd_sum(ss) / float(a.d) + a.eps);
    for (uint i = lane; i < a.d; i += SIMD) {
        x[i] = w[i] * x[i] * inv;
    }
}

// dst[r * dst_stride + i] = src[r * src_stride + i] for i < width, over `rows` rows.
struct CopyArgs {
    uint rows;
    uint width;
    uint src_stride;
    uint dst_stride;
};

kernel void copy_rows(
    device const float *src [[buffer(0)]],
    device float *dst [[buffer(1)]],
    constant CopyArgs &a [[buffer(2)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid < a.rows * a.width) {
        const uint r = gid / a.width;
        const uint i = gid % a.width;
        dst[(ulong)r * a.dst_stride + i] = src[(ulong)r * a.src_stride + i];
    }
}
