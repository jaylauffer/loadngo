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
#include <metal_simdgroup_matrix>
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

// Causal grouped-query attention with an optional sliding window (Gemma 4's layers).
// Layouts, float32: q and out [t][heads][dim]; k and v [slots][kv_heads][dim], where
// position s lives in row s % slots (a ring when slots is smaller than the positions).
// New position i (absolute start + i) attends to positions max(0, start + i + 1 - window)
// ..= start + i, head h to KV head h / (heads / kv_heads). Needs dim a multiple of 32 and
// at most 512, and slots >= window + t when slots < start + t.
//
// One threadgroup per (new position, head). Its eight simdgroups take interleaved runs
// of the positions, each keeping an online softmax (running maximum, sum and weighted
// values, lanes holding dim / 32 elements); the eight partial results are merged in
// threadgroup memory. In float32 throughout.
//
// With `sinks` set, head h's softmax has one more logit, sinks[h] (gpt-oss's attention
// sinks): it takes probability but adds no value. Otherwise the sinks buffer is not read.
struct GroupedArgs {
    uint t;
    uint start;
    uint heads;
    uint kv_heads;
    uint dim;
    uint window;
    uint slots;
    float scale;
    uint sinks;
};

kernel void attention_grouped(
    device const float *q [[buffer(0)]],
    device const float *k [[buffer(1)]],
    device const float *v [[buffer(2)]],
    device float *out [[buffer(3)]],
    device const float *sinks [[buffer(4)]],
    constant GroupedArgs &a [[buffer(5)]],
    uint group [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]],
    uint tid [[thread_index_in_threadgroup]])
{
    threadgroup float part_max[8];
    threadgroup float part_sum[8];
    threadgroup float part_acc[8 * 512];
    const uint step = group / a.heads;
    const uint head = group % a.heads;
    const uint kv_head = head / (a.heads / a.kv_heads);
    const uint n = a.dim / SIMD;
    device const float *qt = q + ((ulong)step * a.heads + head) * a.dim;
    float qv[16];
    float acc[16];
    for (uint i = 0; i < 16; ++i) {
        qv[i] = i < n ? qt[lane + SIMD * i] : 0.0f;
        acc[i] = 0.0f;
    }
    const uint last = a.start + step;
    const uint first = last + 1 > a.window ? last + 1 - a.window : 0;
    const ulong row = (ulong)a.kv_heads * a.dim;
    float m = -INFINITY;
    float l = 0.0f;
    for (uint s = first + sg; s <= last; s += 8) {
        const ulong at = (ulong)(s % a.slots) * row + (ulong)kv_head * a.dim;
        device const float *kr = k + at;
        device const float *vr = v + at;
        float dot = 0.0f;
        for (uint i = 0; i < n; ++i) {
            dot += qv[i] * kr[lane + SIMD * i];
        }
        const float score = simd_sum(dot) * a.scale;
        const float next = max(m, score);
        const float keep = exp(m - next);
        const float p = exp(score - next);
        l = l * keep + p;
        for (uint i = 0; i < n; ++i) {
            acc[i] = acc[i] * keep + p * vr[lane + SIMD * i];
        }
        m = next;
    }
    if (lane == 0) {
        part_max[sg] = m;
        part_sum[sg] = l;
    }
    for (uint i = 0; i < n; ++i) {
        part_acc[sg * 512 + lane + SIMD * i] = acc[i];
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);
    const float sink = a.sinks != 0 ? sinks[head] : -INFINITY;
    float total_max = sink;
    for (uint g = 0; g < 8; ++g) {
        total_max = max(total_max, part_max[g]);
    }
    float total = sink == -INFINITY ? 0.0f : exp(sink - total_max);
    float weight[8];
    for (uint g = 0; g < 8; ++g) {
        // A simdgroup that saw no positions has maximum -inf and weight 0.
        weight[g] = part_max[g] == -INFINITY ? 0.0f : exp(part_max[g] - total_max);
        total += part_sum[g] * weight[g];
    }
    device float *o = out + ((ulong)step * a.heads + head) * a.dim;
    for (uint d = tid; d < a.dim; d += 256) {
        float sum = 0.0f;
        for (uint g = 0; g < 8; ++g) {
            sum += part_acc[g * 512 + d] * weight[g];
        }
        o[d] = sum / total;
    }
}

// attention_grouped for many new positions, on the matrix units (a prompt pass): a
// threadgroup takes 32 new positions of one head and walks the keys its queries can see
// 32 at a time, as attention_split_key_tiled does. Scores Q K^T (32 x 32) come from 8 x 8
// matrix products; the softmax runs online per query row in threadgroup memory; the
// output (32 x dim) stays in the simdgroups' matrices, rescaled by a diagonal matrix when
// a row's maximum grows, plus P V. Needs dim a multiple of 16, at most 512. Reads rows up
// to the next multiple of 32: q and out hold t rounded up; k and v hold the rows of
// positions up to start + t rounded up to 32, which must be finite; when positions wrap
// (slots < start + t), slots must be a multiple of 32 so a tile of 32 keys never wraps.
kernel void attention_grouped_tiled(
    device const float *q [[buffer(0)]],
    device const float *k [[buffer(1)]],
    device const float *v [[buffer(2)]],
    device float *out [[buffer(3)]],
    device const float *sinks [[buffer(4)]],
    constant GroupedArgs &a [[buffer(5)]],
    uint group [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]],
    uint tid [[thread_index_in_threadgroup]])
{
    threadgroup float scores[32 * 32];
    threadgroup float diag[4 * 64];
    threadgroup float row_max[32];
    threadgroup float row_sum[32];
    const uint head = group % a.heads;
    const uint q0 = (group / a.heads) * 32;
    const uint kv_head = head / (a.heads / a.kv_heads);
    const ulong q_stride = (ulong)a.heads * a.dim;
    const ulong kv_stride = (ulong)a.kv_heads * a.dim;
    // Simdgroup sg: query tile qi (8 queries), score key tiles kj0, kj0 + 1, and value
    // columns vc0 .. vc0 + dim / 2 of the output.
    const uint qi = sg / 2;
    const uint kj0 = (sg % 2) * 2;
    const uint half_dim = a.dim / 2;
    const uint vc0 = (sg % 2) * half_dim;
    const uint tiles = half_dim / 8;
    for (uint i = tid; i < 4 * 64; i += 256) {
        diag[i] = 0.0f;
    }
    if (tid < 32) {
        // A sink starts every row's softmax: its logit is the first maximum, exp(0) = 1
        // the first sum.
        row_max[tid] = a.sinks != 0 ? sinks[head] : -INFINITY;
        row_sum[tid] = a.sinks != 0 ? 1.0f : 0.0f;
    }
    simdgroup_float8x8 o[32];
    for (uint t = 0; t < 32; ++t) {
        o[t] = simdgroup_float8x8(0.0f);
    }
    const uint queries = min(32u, a.t - q0);
    const uint first_query = a.start + q0;
    const uint keys_from = first_query + 1 > a.window ? first_query + 1 - a.window : 0;
    const uint keys_end = first_query + queries;
    device const float *qt = q + (ulong)(q0 + qi * 8) * q_stride + (ulong)head * a.dim;
    for (uint s0 = keys_from / 32 * 32; s0 < keys_end; s0 += 32) {
        const ulong base = (ulong)(s0 % a.slots) * kv_stride + (ulong)kv_head * a.dim;
        simdgroup_float8x8 sc[2] = {simdgroup_float8x8(0.0f), simdgroup_float8x8(0.0f)};
        for (uint d = 0; d < a.dim; d += 8) {
            simdgroup_float8x8 qm;
            simdgroup_load(qm, qt + d, q_stride);
            for (uint j = 0; j < 2; ++j) {
                simdgroup_float8x8 km;
                simdgroup_load(km, k + base + (ulong)((kj0 + j) * 8) * kv_stride + d,
                               kv_stride, ulong2(0, 0), true);
                simdgroup_multiply_accumulate(sc[j], qm, km, sc[j]);
            }
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        for (uint j = 0; j < 2; ++j) {
            simdgroup_store(sc[j], scores, 32, ulong2((kj0 + j) * 8, qi * 8));
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        // Online softmax: 8 threads per query row, 4 keys each.
        {
            const uint r = tid / 8;
            const uint c0 = (tid % 8) * 4;
            const uint last = first_query + r;
            const uint first = last + 1 > a.window ? last + 1 - a.window : 0;
            float p[4];
            float local = -INFINITY;
            for (uint c = 0; c < 4; ++c) {
                const uint s = s0 + c0 + c;
                const bool live = r < queries && s >= first && s <= last;
                p[c] = live ? scores[r * 32 + c0 + c] * a.scale : -INFINITY;
                local = max(local, p[c]);
            }
            local = max(local, simd_shuffle_xor(local, 1));
            local = max(local, simd_shuffle_xor(local, 2));
            local = max(local, simd_shuffle_xor(local, 4));
            const float old = row_max[r];
            const float next = max(old, local);
            float sum = 0.0f;
            for (uint c = 0; c < 4; ++c) {
                p[c] = next == -INFINITY ? 0.0f : exp(p[c] - next);
                sum += p[c];
                scores[r * 32 + c0 + c] = p[c];
            }
            sum += simd_shuffle_xor(sum, 1);
            sum += simd_shuffle_xor(sum, 2);
            sum += simd_shuffle_xor(sum, 4);
            const float keep = next == -INFINITY ? 1.0f : exp(old - next);
            simdgroup_barrier(mem_flags::mem_threadgroup);
            if (tid % 8 == 0) {
                row_max[r] = next;
                row_sum[r] = row_sum[r] * keep + sum;
                diag[(r / 8) * 64 + (r % 8) * 9] = keep;
            }
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        // o = diag(keep) o + P V over this simdgroup's value tiles.
        simdgroup_float8x8 dm;
        simdgroup_load(dm, diag + qi * 64, 8);
        for (uint t = 0; t < tiles; ++t) {
            simdgroup_multiply(o[t], dm, o[t]);
        }
        for (uint kk = 0; kk < 32; kk += 8) {
            simdgroup_float8x8 pm;
            simdgroup_load(pm, scores, 32, ulong2(kk, qi * 8));
            device const float *vt = v + base + (ulong)kk * kv_stride + vc0;
            for (uint t = 0; t < tiles; ++t) {
                simdgroup_float8x8 vm;
                simdgroup_load(vm, vt + t * 8, kv_stride);
                simdgroup_multiply_accumulate(o[t], pm, vm, o[t]);
            }
        }
    }
    // out = diag(1 / sum) o.
    threadgroup_barrier(mem_flags::mem_threadgroup);
    if (tid < 32) {
        const uint r = tid;
        diag[(r / 8) * 64 + (r % 8) * 9] = row_sum[r] > 0.0f ? 1.0f / row_sum[r] : 0.0f;
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);
    simdgroup_float8x8 dm;
    simdgroup_load(dm, diag + qi * 64, 8);
    device float *ot = out + (ulong)(q0 + qi * 8) * q_stride + (ulong)head * a.dim + vc0;
    for (uint t = 0; t < tiles; ++t) {
        simdgroup_multiply(o[t], dm, o[t]);
        simdgroup_store(o[t], ot + t * 8, q_stride);
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

// y[p] = W x[p] for n positions on the matrix units, W in MXFP4: a threadgroup computes a
// tile of 64 weight rows by 32 positions, 32 columns (one MXFP4 block) at a time. The
// weight tile is decoded once into threadgroup memory as float and shared by all 32
// positions, so each weight is read once per 32 positions. Needs rows % 64 == 0,
// cols % 32 == 0 and n % 32 == 0 (callers pad positions); x rows start on 16-byte
// boundaries. Products and sums in float32.
constant constexpr uint TILE_M = 64;
constant constexpr uint TILE_N = 32;
constant constexpr uint TILE_K = 32;

kernel void gemm_mxfp4_tiled(
    device const uchar *elements [[buffer(0)]],
    device const uchar *scales [[buffer(1)]],
    device const float *x [[buffer(2)]],
    device float *y [[buffer(3)]],
    constant GemmArgs &a [[buffer(4)]],
    uint group [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint tid [[thread_index_in_threadgroup]])
{
    threadgroup float lut[16];
    threadgroup float wa[TILE_M * TILE_K];
    threadgroup float xb[TILE_N * TILE_K];
    if (tid < 16) {
        lut[tid] = E2M1[tid];
    }
    const uint row_tiles = a.rows / TILE_M;
    const uint r0 = (group % row_tiles) * TILE_M;
    const uint p0 = (group / row_tiles) * TILE_N;
    const uint blocks = a.cols / 32;
    // Simdgroup sg owns rows (sg % 4) * 16 .. +16 and positions (sg / 4) * 16 .. +16.
    const uint rs = (sg % 4) * 16;
    const uint ps = (sg / 4) * 16;
    simdgroup_float8x8 acc[2][2];
    for (uint i = 0; i < 2; ++i) {
        for (uint j = 0; j < 2; ++j) {
            acc[i][j] = simdgroup_float8x8(0.0f);
        }
    }
    // Loading: thread t decodes 8 weights (row t / 4, word t % 4 of the block) and loads
    // 4 x values (position t / 8, floats (t % 8) * 4 .. +4).
    const uint wrow = tid / 4;
    const uint word = tid % 4;
    const uint xpos = tid / 8;
    const uint xpart = tid % 8;
    device const uint *codes = (device const uint *)elements;
    for (uint b = 0; b < blocks; ++b) {
        threadgroup_barrier(mem_flags::mem_threadgroup);
        {
            const ulong row = r0 + wrow;
            const uint q = codes[(row * blocks + b) * 4 + word];
            const float scale = e8m0(scales[row * blocks + b]);
            threadgroup float *dst = wa + wrow * TILE_K + word * 8;
            for (uint i = 0; i < 8; ++i) {
                dst[i] = lut[(q >> (4 * i)) & 15] * scale;
            }
            device const float4 *src =
                (device const float4 *)(x + (ulong)(p0 + xpos) * a.x_stride + b * TILE_K);
            ((threadgroup float4 *)(xb + xpos * TILE_K))[xpart] = src[xpart];
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        for (uint k = 0; k < TILE_K; k += 8) {
            simdgroup_float8x8 wm[2];
            simdgroup_float8x8 xm[2];
            for (uint i = 0; i < 2; ++i) {
                simdgroup_load(wm[i], wa, TILE_K, ulong2(k, rs + i * 8));
                // Positions are rows of xb; as a K x N matrix they are its columns.
                simdgroup_load(xm[i], xb, TILE_K, ulong2(k, ps + i * 8), true);
            }
            for (uint i = 0; i < 2; ++i) {
                for (uint j = 0; j < 2; ++j) {
                    simdgroup_multiply_accumulate(acc[i][j], wm[i], xm[j], acc[i][j]);
                }
            }
        }
    }
    // acc[i][j] is rows x positions; y is positions x rows: store it transposed.
    for (uint i = 0; i < 2; ++i) {
        for (uint j = 0; j < 2; ++j) {
            device float *dst = y + (ulong)(p0 + ps + j * 8) * a.y_stride + r0 + rs + i * 8;
            simdgroup_store(acc[i][j], dst, a.y_stride, ulong2(0, 0), true);
        }
    }
}

// As gemm_mxfp4_tiled for a bfloat16 W (row-major, little-endian): the 64 x 32 weight
// tile is widened into threadgroup memory once and shared by 32 positions. Needs
// rows % 64 == 0, cols % 32 == 0 and n % 32 == 0; W rows and x rows on 16-byte
// boundaries (cols % 8 == 0 keeps them there).
kernel void gemm_bf16_tiled(
    device const uchar *w [[buffer(0)]],
    device const float *x [[buffer(1)]],
    device float *y [[buffer(2)]],
    constant GemmArgs &a [[buffer(3)]],
    uint group [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint tid [[thread_index_in_threadgroup]])
{
    threadgroup float wa[TILE_M * TILE_K];
    threadgroup float xb[TILE_N * TILE_K];
    const uint row_tiles = a.rows / TILE_M;
    const uint r0 = (group % row_tiles) * TILE_M;
    const uint p0 = (group / row_tiles) * TILE_N;
    const uint rs = (sg % 4) * 16;
    const uint ps = (sg / 4) * 16;
    simdgroup_float8x8 acc[2][2];
    for (uint i = 0; i < 2; ++i) {
        for (uint j = 0; j < 2; ++j) {
            acc[i][j] = simdgroup_float8x8(0.0f);
        }
    }
    // Thread t widens 8 weights (row t / 4, elements (t % 4) * 8 .. +8: one uint4) and
    // loads 4 x values (position t / 8, floats (t % 8) * 4 .. +4).
    const uint wrow = tid / 4;
    const uint part = tid % 4;
    const uint xpos = tid / 8;
    const uint xpart = tid % 8;
    device const uint4 *w4 = (device const uint4 *)w;
    const uint row_words = a.cols / 8;
    for (uint k0 = 0; k0 < a.cols; k0 += TILE_K) {
        threadgroup_barrier(mem_flags::mem_threadgroup);
        {
            const uint4 q = w4[(ulong)(r0 + wrow) * row_words + k0 / 8 + part];
            threadgroup float *dst = wa + wrow * TILE_K + part * 8;
            dst[0] = bf16_lo(q.x);
            dst[1] = bf16_hi(q.x);
            dst[2] = bf16_lo(q.y);
            dst[3] = bf16_hi(q.y);
            dst[4] = bf16_lo(q.z);
            dst[5] = bf16_hi(q.z);
            dst[6] = bf16_lo(q.w);
            dst[7] = bf16_hi(q.w);
            device const float4 *src =
                (device const float4 *)(x + (ulong)(p0 + xpos) * a.x_stride + k0);
            ((threadgroup float4 *)(xb + xpos * TILE_K))[xpart] = src[xpart];
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        for (uint k = 0; k < TILE_K; k += 8) {
            simdgroup_float8x8 wm[2];
            simdgroup_float8x8 xm[2];
            for (uint i = 0; i < 2; ++i) {
                simdgroup_load(wm[i], wa, TILE_K, ulong2(k, rs + i * 8));
                simdgroup_load(xm[i], xb, TILE_K, ulong2(k, ps + i * 8), true);
            }
            for (uint i = 0; i < 2; ++i) {
                for (uint j = 0; j < 2; ++j) {
                    simdgroup_multiply_accumulate(acc[i][j], wm[i], xm[j], acc[i][j]);
                }
            }
        }
    }
    for (uint i = 0; i < 2; ++i) {
        for (uint j = 0; j < 2; ++j) {
            device float *dst = y + (ulong)(p0 + ps + j * 8) * a.y_stride + r0 + rs + i * 8;
            simdgroup_store(acc[i][j], dst, a.y_stride, ulong2(0, 0), true);
        }
    }
}

// attention_split_key for many new positions, on the matrix units: a threadgroup takes 32
// new positions (queries) of one head and walks the cache 32 keys at a time. Scores
// Q K^T (32 x 32) come from 8 x 8 matrix products; the softmax runs online per query row
// in threadgroup memory; the output (32 x dv, dv = 128) stays in the simdgroups' matrices,
// rescaled by a diagonal matrix when a row's maximum grows, plus P V. Needs qa and qb
// multiples of 8, dv == 128. Reads (and writes) rows up to the next multiple of 32: q and
// out need t rounded up, kv and shared cached + t rounded up, and cache rows past
// cached + t must be finite (their weight is exactly zero).
kernel void attention_split_key_tiled(
    device const float *q [[buffer(0)]],
    device const float *kv [[buffer(1)]],
    device const float *shared_key [[buffer(2)]],
    device float *out [[buffer(3)]],
    constant AttentionArgs &a [[buffer(4)]],
    uint group [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]],
    uint tid [[thread_index_in_threadgroup]])
{
    threadgroup float scores[32 * 32];
    threadgroup float diag[4 * 64];
    threadgroup float row_max[32];
    threadgroup float row_sum[32];
    const uint head = group % a.heads;
    const uint q0 = (group / a.heads) * 32;
    const uint dq = a.qa + a.qb;
    const uint row = a.qa + a.dv;
    const ulong q_stride = (ulong)a.heads * dq;
    const ulong kv_stride = (ulong)a.heads * row;
    // Simdgroup sg: query tile qi (8 queries), score key tiles kj0, kj0 + 1, and value
    // columns vc0 .. vc0 + 64 of the output.
    const uint qi = sg / 2;
    const uint kj0 = (sg % 2) * 2;
    const uint vc0 = (sg % 2) * 64;
    for (uint i = tid; i < 4 * 64; i += 256) {
        diag[i] = 0.0f;
    }
    if (tid < 32) {
        row_max[tid] = -INFINITY;
        row_sum[tid] = 0.0f;
    }
    simdgroup_float8x8 o[8];
    for (uint v = 0; v < 8; ++v) {
        o[v] = simdgroup_float8x8(0.0f);
    }
    const uint queries = min(32u, a.t - q0);
    const uint keys_end = a.cached + q0 + queries;
    device const float *qt = q + (ulong)(q0 + qi * 8) * q_stride + (ulong)head * dq;
    for (uint s0 = 0; s0 < keys_end; s0 += 32) {
        // Scores for this simdgroup's two 8 x 8 tiles.
        simdgroup_float8x8 sc[2] = {simdgroup_float8x8(0.0f), simdgroup_float8x8(0.0f)};
        for (uint d = 0; d < dq; d += 8) {
            simdgroup_float8x8 qm;
            simdgroup_load(qm, qt + d, q_stride);
            for (uint j = 0; j < 2; ++j) {
                const uint s = s0 + (kj0 + j) * 8;
                simdgroup_float8x8 km;
                if (d < a.qa) {
                    simdgroup_load(km, kv + (ulong)s * kv_stride + (ulong)head * row + d,
                                   kv_stride, ulong2(0, 0), true);
                } else {
                    simdgroup_load(km, shared_key + (ulong)s * a.qb + (d - a.qa), a.qb,
                                   ulong2(0, 0), true);
                }
                simdgroup_multiply_accumulate(sc[j], qm, km, sc[j]);
            }
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        for (uint j = 0; j < 2; ++j) {
            simdgroup_store(sc[j], scores, 32, ulong2((kj0 + j) * 8, qi * 8));
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        // Online softmax: 8 threads per query row, 4 keys each.
        {
            const uint r = tid / 8;
            const uint c0 = (tid % 8) * 4;
            const uint limit = a.cached + q0 + r; // last key this query sees
            float p[4];
            float local = -INFINITY;
            for (uint c = 0; c < 4; ++c) {
                const uint s = s0 + c0 + c;
                const bool live = r < queries && s <= limit;
                p[c] = live ? scores[r * 32 + c0 + c] * a.scale : -INFINITY;
                local = max(local, p[c]);
            }
            local = max(local, simd_shuffle_xor(local, 1));
            local = max(local, simd_shuffle_xor(local, 2));
            local = max(local, simd_shuffle_xor(local, 4));
            const float old = row_max[r];
            const float next = max(old, local);
            float sum = 0.0f;
            for (uint c = 0; c < 4; ++c) {
                p[c] = next == -INFINITY ? 0.0f : exp(p[c] - next);
                sum += p[c];
                scores[r * 32 + c0 + c] = p[c];
            }
            sum += simd_shuffle_xor(sum, 1);
            sum += simd_shuffle_xor(sum, 2);
            sum += simd_shuffle_xor(sum, 4);
            const float keep = next == -INFINITY ? 1.0f : exp(old - next);
            // Every lane read row_max[r] above; one writes it after the barrier-free
            // shuffles, and the value it writes is what all eight computed.
            simdgroup_barrier(mem_flags::mem_threadgroup);
            if (tid % 8 == 0) {
                row_max[r] = next;
                row_sum[r] = row_sum[r] * keep + sum;
                diag[(r / 8) * 64 + (r % 8) * 9] = keep;
            }
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        // o = diag(keep) o + P V over this simdgroup's 8 value tiles.
        simdgroup_float8x8 dm;
        simdgroup_load(dm, diag + qi * 64, 8);
        for (uint v = 0; v < 8; ++v) {
            simdgroup_multiply(o[v], dm, o[v]);
        }
        for (uint k = 0; k < 32; k += 8) {
            simdgroup_float8x8 pm;
            simdgroup_load(pm, scores, 32, ulong2(k, qi * 8));
            device const float *vt = kv + (ulong)(s0 + k) * kv_stride + (ulong)head * row
                                   + a.qa + vc0;
            for (uint v = 0; v < 8; ++v) {
                simdgroup_float8x8 vm;
                simdgroup_load(vm, vt + v * 8, kv_stride);
                simdgroup_multiply_accumulate(o[v], pm, vm, o[v]);
            }
        }
    }
    // out = diag(1 / sum) o.
    threadgroup_barrier(mem_flags::mem_threadgroup);
    if (tid < 32) {
        const uint r = tid;
        diag[(r / 8) * 64 + (r % 8) * 9] = row_sum[r] > 0.0f ? 1.0f / row_sum[r] : 0.0f;
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);
    simdgroup_float8x8 dm;
    simdgroup_load(dm, diag + qi * 64, 8);
    device float *ot = out + (ulong)(q0 + qi * 8) * a.heads * a.dv + (ulong)head * a.dv + vc0;
    for (uint v = 0; v < 8; ++v) {
        simdgroup_multiply(o[v], dm, o[v]);
        simdgroup_store(o[v], ot + v * 8, (ulong)a.heads * a.dv);
    }
}

// ggml Q8_0, repacked for aligned loads: `codes` holds each row's signed bytes in order
// (rows * cols), `scales` one binary16 per 32 of them (rows * cols / 32); element c of
// row r is scales[r * cols / 32 + c / 32] * codes[r * cols + c]. Needs cols % 32 == 0.
// The same structure as the MXFP4 kernels above.
template <uint ROWS>
kernel void gemv_q8_0(
    device const char *codes [[buffer(0)]],
    device const half *scales [[buffer(1)]],
    device const float *x [[buffer(2)]],
    device float *y [[buffer(3)]],
    constant GemvArgs &a [[buffer(4)]],
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
    const uint blocks = a.cols / 32;
    device const char4 *c4 = (device const char4 *)codes;
    device const float4 *x4 = (device const float4 *)x;
    float acc[ROWS];
    for (uint r = 0; r < ROWS; ++r) {
        acc[r] = 0.0f;
    }
    for (uint b = lane; b < blocks; b += SIMD) {
        float4 xs[8];
        for (uint i = 0; i < 8; ++i) {
            xs[i] = x4[8 * b + i];
        }
        for (uint r = 0; r < ROWS; ++r) {
            if (r < count) {
                const ulong row = first + r;
                device const char4 *w = c4 + row * (a.cols / 4) + 8 * b;
                float block = 0.0f;
                for (uint i = 0; i < 8; ++i) {
                    block += dot(float4(w[i]), xs[i]);
                }
                acc[r] += block * float(scales[row * blocks + b]);
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

template [[host_name("gemv_q8_0_r1")]] kernel void gemv_q8_0<1>(
    device const char *, device const half *, device const float *, device float *,
    constant GemvArgs &, uint, uint, uint, uint);
template [[host_name("gemv_q8_0_r2")]] kernel void gemv_q8_0<2>(
    device const char *, device const half *, device const float *, device float *,
    constant GemvArgs &, uint, uint, uint, uint);
template [[host_name("gemv_q8_0_r4")]] kernel void gemv_q8_0<4>(
    device const char *, device const half *, device const float *, device float *,
    constant GemvArgs &, uint, uint, uint, uint);

// y[p] = W x[p] for n positions, W in repacked Q8_0, GEMM_N positions per pass over W.
kernel void gemm_q8_0(
    device const char *codes [[buffer(0)]],
    device const half *scales [[buffer(1)]],
    device const float *x [[buffer(2)]],
    device float *y [[buffer(3)]],
    constant GemmArgs &a [[buffer(4)]],
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
    const uint blocks = a.cols / 32;
    device const char4 *c4 = (device const char4 *)codes;
    for (uint p0 = 0; p0 < a.n; p0 += GEMM_N) {
        const uint np = min(GEMM_N, a.n - p0);
        float acc[GEMM_ROWS][GEMM_N];
        for (uint r = 0; r < GEMM_ROWS; ++r) {
            for (uint p = 0; p < GEMM_N; ++p) {
                acc[r][p] = 0.0f;
            }
        }
        for (uint b = lane; b < blocks; b += SIMD) {
            for (uint r = 0; r < GEMM_ROWS; ++r) {
                if (r < count) {
                    const ulong row = first + r;
                    device const char4 *w = c4 + row * (a.cols / 4) + 8 * b;
                    const float scale = float(scales[row * blocks + b]);
                    float4 wv[8];
                    for (uint i = 0; i < 8; ++i) {
                        wv[i] = float4(w[i]);
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

// gemm_q8_0 on the matrix units, as gemm_mxfp4_tiled: a 64-row by 32-position tile, the
// weights of one 32-column block widened into threadgroup memory per step. Needs
// rows % 64 == 0, cols % 32 == 0 and n % 32 == 0.
kernel void gemm_q8_0_tiled(
    device const char *codes [[buffer(0)]],
    device const half *scales [[buffer(1)]],
    device const float *x [[buffer(2)]],
    device float *y [[buffer(3)]],
    constant GemmArgs &a [[buffer(4)]],
    uint group [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint tid [[thread_index_in_threadgroup]])
{
    threadgroup float wa[TILE_M * TILE_K];
    threadgroup float xb[TILE_N * TILE_K];
    const uint row_tiles = a.rows / TILE_M;
    const uint r0 = (group % row_tiles) * TILE_M;
    const uint p0 = (group / row_tiles) * TILE_N;
    const uint blocks = a.cols / 32;
    const uint rs = (sg % 4) * 16;
    const uint ps = (sg / 4) * 16;
    simdgroup_float8x8 acc[2][2];
    for (uint i = 0; i < 2; ++i) {
        for (uint j = 0; j < 2; ++j) {
            acc[i][j] = simdgroup_float8x8(0.0f);
        }
    }
    // Thread t widens 8 weights (row t / 4, eighth t % 4 of the block) and loads 4 x
    // values (position t / 8, floats (t % 8) * 4 .. +4).
    const uint wrow = tid / 4;
    const uint part = tid % 4;
    const uint xpos = tid / 8;
    const uint xpart = tid % 8;
    device const char4 *c4 = (device const char4 *)codes;
    for (uint b = 0; b < blocks; ++b) {
        threadgroup_barrier(mem_flags::mem_threadgroup);
        {
            const ulong row = r0 + wrow;
            const float scale = float(scales[row * blocks + b]);
            device const char4 *w = c4 + row * (a.cols / 4) + 8 * b + 2 * part;
            const float4 lo = float4(w[0]) * scale;
            const float4 hi = float4(w[1]) * scale;
            threadgroup float4 *dst = (threadgroup float4 *)(wa + wrow * TILE_K + part * 8);
            dst[0] = lo;
            dst[1] = hi;
            device const float4 *src =
                (device const float4 *)(x + (ulong)(p0 + xpos) * a.x_stride + b * TILE_K);
            ((threadgroup float4 *)(xb + xpos * TILE_K))[xpart] = src[xpart];
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        for (uint k = 0; k < TILE_K; k += 8) {
            simdgroup_float8x8 wm[2];
            simdgroup_float8x8 xm[2];
            for (uint i = 0; i < 2; ++i) {
                simdgroup_load(wm[i], wa, TILE_K, ulong2(k, rs + i * 8));
                simdgroup_load(xm[i], xb, TILE_K, ulong2(k, ps + i * 8), true);
            }
            for (uint i = 0; i < 2; ++i) {
                for (uint j = 0; j < 2; ++j) {
                    simdgroup_multiply_accumulate(acc[i][j], wm[i], xm[j], acc[i][j]);
                }
            }
        }
    }
    for (uint i = 0; i < 2; ++i) {
        for (uint j = 0; j < 2; ++j) {
            device float *dst = y + (ulong)(p0 + ps + j * 8) * a.y_stride + r0 + rs + i * 8;
            simdgroup_store(acc[i][j], dst, a.y_stride, ulong2(0, 0), true);
        }
    }
}

// dst[r][i] += src[r * src_stride + i] over `rows` rows of `width` floats; a src_stride
// of 0 adds the same row (a bias) to every row.
struct AddArgs {
    uint rows;
    uint width;
    uint src_stride;
};

kernel void add_rows(
    device float *dst [[buffer(0)]],
    device const float *src [[buffer(1)]],
    constant AddArgs &a [[buffer(2)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid < a.rows * a.width) {
        const uint r = gid / a.width;
        const uint i = gid % a.width;
        dst[gid] += src[(ulong)r * a.src_stride + i];
    }
}

// Rotary position embedding by halves, in place: x is [rows][heads][dim]; table holds,
// per row, dim / 2 pairs (cos, sin). Element i of each head's first half turns with
// element i of its second half by that pair.
struct RotateArgs {
    uint rows;
    uint heads;
    uint dim;
};

kernel void rotate_halves(
    device float *x [[buffer(0)]],
    device const float2 *table [[buffer(1)]],
    constant RotateArgs &a [[buffer(2)]],
    uint gid [[thread_position_in_grid]])
{
    const uint half_dim = a.dim / 2;
    if (gid < a.rows * a.heads * half_dim) {
        const uint i = gid % half_dim;
        const uint row_head = gid / half_dim;
        const uint row = row_head / a.heads;
        device float *h = x + (ulong)row_head * a.dim;
        const float2 cs = table[(ulong)row * half_dim + i];
        const float x0 = h[i];
        const float x1 = h[i + half_dim];
        h[i] = x0 * cs.x - x1 * cs.y;
        h[i + half_dim] = x1 * cs.x + x0 * cs.y;
    }
}

// The clamped SwiGLU of gpt-oss's experts, over `rows` rows of `width`, biases broadcast:
//   g = min(gate + gate_bias, limit),  u = clamp(up + up_bias, -limit, limit)
//   out = (u + 1) * g * sigmoid(alpha * g)
struct GluArgs {
    uint rows;
    uint width;
    float limit;
    float alpha;
};

kernel void clamped_swiglu(
    device const float *gate [[buffer(0)]],
    device const float *up [[buffer(1)]],
    device const float *gate_bias [[buffer(2)]],
    device const float *up_bias [[buffer(3)]],
    device float *out [[buffer(4)]],
    constant GluArgs &a [[buffer(5)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid < a.rows * a.width) {
        const uint i = gid % a.width;
        const float g = min(gate[gid] + gate_bias[i], a.limit);
        const float u = clamp(up[gid] + up_bias[i], -a.limit, a.limit);
        out[gid] = (u + 1.0f) * g / (1.0f + exp(-a.alpha * g));
    }
}
