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
