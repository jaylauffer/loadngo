#!/usr/bin/env python3
"""Writes gpt-oss/tests/fixtures/tiny/: a random-weight gpt-oss with the 20B's structure
at toy size, as a GGUF (Q8_0 attention, embedding and output; MXFP4 experts; f32 norms,
biases, sinks and router, as in the real file), and transformers' own logits for it --
the oracle for gpt-oss/tests/tiny_oracle.rs.

transformers runs on exactly the quantized weights, decoded back by ggml's `gguf`
package, so only the forward pass differs between the two.

    python3 scripts/gpt_oss_tiny_oracle.py      # needs torch, transformers, gguf
"""
import json
import pathlib

import numpy as np
import torch
from gguf import GGMLQuantizationType as Q, GGUFWriter
from gguf.quants import dequantize, quantize
from transformers import GptOssConfig, GptOssForCausalLM

LAYERS, HIDDEN, HEADS, KV_HEADS, HEAD_DIM = 4, 128, 4, 2, 32
EXPERTS, USED, EXPERT_HIDDEN, VOCAB = 4, 2, 64, 512
WINDOW, THETA, FACTOR, ORIGINAL = 8, 150000.0, 32.0, 64
TOKENS = 40

out = pathlib.Path(__file__).resolve().parent.parent / "gpt-oss/tests/fixtures/tiny"
out.mkdir(parents=True, exist_ok=True)
rng = np.random.default_rng(20261004)

def normal(shape, std):
    return (rng.standard_normal(shape) * std).astype(np.float32)

writer = GGUFWriter(str(out / "model.gguf"), "gpt-oss")
writer.add_uint32("gpt-oss.block_count", LAYERS)
writer.add_uint32("gpt-oss.context_length", 2048)
writer.add_uint32("gpt-oss.embedding_length", HIDDEN)
writer.add_uint32("gpt-oss.feed_forward_length", EXPERT_HIDDEN)
writer.add_uint32("gpt-oss.attention.head_count", HEADS)
writer.add_uint32("gpt-oss.attention.head_count_kv", KV_HEADS)
writer.add_float32("gpt-oss.rope.freq_base", THETA)
writer.add_float32("gpt-oss.attention.layer_norm_rms_epsilon", 1e-5)
writer.add_uint32("gpt-oss.expert_count", EXPERTS)
writer.add_uint32("gpt-oss.expert_used_count", USED)
writer.add_uint32("gpt-oss.attention.key_length", HEAD_DIM)
writer.add_uint32("gpt-oss.attention.value_length", HEAD_DIM)
writer.add_uint32("gpt-oss.attention.sliding_window", WINDOW)
writer.add_uint32("gpt-oss.expert_feed_forward_length", EXPERT_HIDDEN)
writer.add_string("gpt-oss.rope.scaling.type", "yarn")
writer.add_float32("gpt-oss.rope.scaling.factor", FACTOR)
writer.add_uint32("gpt-oss.rope.scaling.original_context_length", ORIGINAL)

def put(name, array, kind):
    """Stores `array` as `kind` and returns what ggml decodes it back to."""
    if kind == Q.F32:
        writer.add_tensor(name, array)
        return array
    packed = quantize(array, kind)
    writer.add_tensor(name, packed, raw_shape=packed.shape, raw_dtype=kind)
    return dequantize(packed, kind).reshape(array.shape).astype(np.float32)

weights = {}
weights["embed"] = put("token_embd.weight", normal((VOCAB, HIDDEN), 1.0), Q.Q8_0)
weights["output"] = put("output.weight", normal((VOCAB, HIDDEN), 0.1), Q.Q8_0)
weights["norm"] = put("output_norm.weight", 1 + normal((HIDDEN,), 0.1), Q.F32)
for l in range(LAYERS):
    p = f"blk.{l}."
    w = {}
    w["attn_norm"] = put(p + "attn_norm.weight", 1 + normal((HIDDEN,), 0.1), Q.F32)
    w["q"] = put(p + "attn_q.weight", normal((HEADS * HEAD_DIM, HIDDEN), 0.15), Q.Q8_0)
    w["k"] = put(p + "attn_k.weight", normal((KV_HEADS * HEAD_DIM, HIDDEN), 0.15), Q.Q8_0)
    w["v"] = put(p + "attn_v.weight", normal((KV_HEADS * HEAD_DIM, HIDDEN), 0.15), Q.Q8_0)
    w["o"] = put(p + "attn_output.weight", normal((HIDDEN, HEADS * HEAD_DIM), 0.1), Q.Q8_0)
    w["q_b"] = put(p + "attn_q.bias", normal((HEADS * HEAD_DIM,), 0.1), Q.F32)
    w["k_b"] = put(p + "attn_k.bias", normal((KV_HEADS * HEAD_DIM,), 0.1), Q.F32)
    w["v_b"] = put(p + "attn_v.bias", normal((KV_HEADS * HEAD_DIM,), 0.1), Q.F32)
    w["o_b"] = put(p + "attn_output.bias", normal((HIDDEN,), 0.1), Q.F32)
    w["sinks"] = put(p + "attn_sinks.weight", normal((HEADS,), 1.0), Q.F32)
    w["ffn_norm"] = put(p + "post_attention_norm.weight", 1 + normal((HIDDEN,), 0.1), Q.F32)
    w["router"] = put(p + "ffn_gate_inp.weight", normal((EXPERTS, HIDDEN), 0.3), Q.F32)
    w["router_b"] = put(p + "ffn_gate_inp.bias", normal((EXPERTS,), 0.3), Q.F32)
    # Large enough that the clamps at 7 take effect.
    w["gate"] = put(p + "ffn_gate_exps.weight", normal((EXPERTS, EXPERT_HIDDEN, HIDDEN), 0.5), Q.MXFP4)
    w["up"] = put(p + "ffn_up_exps.weight", normal((EXPERTS, EXPERT_HIDDEN, HIDDEN), 0.5), Q.MXFP4)
    w["down"] = put(p + "ffn_down_exps.weight", normal((EXPERTS, HIDDEN, EXPERT_HIDDEN), 0.1), Q.MXFP4)
    w["gate_b"] = put(p + "ffn_gate_exps.bias", normal((EXPERTS, EXPERT_HIDDEN), 0.1), Q.F32)
    w["up_b"] = put(p + "ffn_up_exps.bias", normal((EXPERTS, EXPERT_HIDDEN), 0.1), Q.F32)
    w["down_b"] = put(p + "ffn_down_exps.bias", normal((EXPERTS, HIDDEN), 0.1), Q.F32)
    weights[l] = w
writer.write_header_to_file()
writer.write_kv_data_to_file()
writer.write_tensors_to_file()
writer.close()

config = GptOssConfig(
    num_hidden_layers=LAYERS, num_local_experts=EXPERTS, vocab_size=VOCAB,
    hidden_size=HIDDEN, intermediate_size=EXPERT_HIDDEN, head_dim=HEAD_DIM,
    num_attention_heads=HEADS, num_key_value_heads=KV_HEADS, sliding_window=WINDOW,
    max_position_embeddings=2048, rms_norm_eps=1e-5, num_experts_per_tok=USED,
    rope_parameters={"rope_type": "yarn", "rope_theta": THETA, "factor": FACTOR,
                     "beta_fast": 32.0, "beta_slow": 1.0, "truncate": False,
                     "original_max_position_embeddings": ORIGINAL},
    attn_implementation="eager",
)
model = GptOssForCausalLM(config).eval()
t = lambda a: torch.tensor(np.asarray(a, dtype=np.float32))
with torch.no_grad():
    model.model.embed_tokens.weight.copy_(t(weights["embed"]))
    model.lm_head.weight.copy_(t(weights["output"]))
    model.model.norm.weight.copy_(t(weights["norm"]))
    for l, layer in enumerate(model.model.layers):
        w = weights[l]
        a = layer.self_attn
        layer.input_layernorm.weight.copy_(t(w["attn_norm"]))
        layer.post_attention_layernorm.weight.copy_(t(w["ffn_norm"]))
        for proj, key in [(a.q_proj, "q"), (a.k_proj, "k"), (a.v_proj, "v"), (a.o_proj, "o")]:
            proj.weight.copy_(t(w[key]))
            proj.bias.copy_(t(w[key + "_b"]))
        a.sinks.copy_(t(w["sinks"]))
        layer.mlp.router.weight.copy_(t(w["router"]))
        layer.mlp.router.bias.copy_(t(w["router_b"]))
        experts = layer.mlp.experts
        # transformers multiplies x @ W with gate and up interleaved in the last axis.
        gate_up = np.zeros((EXPERTS, HIDDEN, 2 * EXPERT_HIDDEN), dtype=np.float32)
        gate_up[:, :, 0::2] = np.transpose(w["gate"], (0, 2, 1))
        gate_up[:, :, 1::2] = np.transpose(w["up"], (0, 2, 1))
        gate_up_b = np.zeros((EXPERTS, 2 * EXPERT_HIDDEN), dtype=np.float32)
        gate_up_b[:, 0::2] = w["gate_b"]
        gate_up_b[:, 1::2] = w["up_b"]
        experts.gate_up_proj.copy_(t(gate_up))
        experts.gate_up_proj_bias.copy_(t(gate_up_b))
        experts.down_proj.copy_(t(np.transpose(w["down"], (0, 2, 1))))
        experts.down_proj_bias.copy_(t(w["down_b"]))

ids = [int(i) for i in rng.integers(0, VOCAB, TOKENS)]
with torch.no_grad():
    logits = model(torch.tensor([ids])).logits[0].float().numpy()
(out / "logits.f32").write_bytes(logits.astype("<f4").tobytes())
json.dump({"ids": ids, "vocab": VOCAB, "positions": TOKENS,
           "transformers": __import__("transformers").__version__},
          open(out / "oracle.json", "w"))
print("logits", logits.shape, "absmax", float(abs(logits).max()), "std", float(logits.std()))
