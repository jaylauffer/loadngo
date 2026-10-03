#!/usr/bin/env python3
"""Writes weights/tests/fixtures/gpt-oss-20b-rows.json: a few rows of gpt-oss-20b's GGUF
tensors dequantised by ggml's own Python reader (the `gguf` package), the reference for
weights/tests/gguf_gpt_oss.rs. The values are gpt-oss-20b weights (Apache 2.0, OpenAI).

    python3 scripts/gguf_reference_rows.py ~/.loadngo/models/56fcc05c...gguf
"""
import json
import sys

import numpy as np
from gguf import GGUFReader
from gguf.quants import dequantize

path = sys.argv[1]
reader = GGUFReader(path)
tensors = {t.name: t for t in reader.tensors}
picks = [
    ("token_embd.weight", [0, 13, 200005]),
    ("blk.0.attn_q.weight", [0, 4095]),
    ("blk.0.attn_sinks.weight", None),
    ("blk.3.ffn_gate_exps.weight", [(0, 0), (31, 2879)]),
    ("blk.23.ffn_down_exps.weight", [(7, 100)]),
]
out = {"header": {"tensor_count": len(reader.tensors)}, "rows": []}
for name, rows in picks:
    t = tensors[name]
    values = dequantize(t.data, t.tensor_type)
    shape = [int(d) for d in t.shape]  # ggml order, first fastest
    values = np.asarray(values, dtype=np.float32).reshape(list(reversed(shape)))
    entry = {"name": name, "dims": shape, "type": int(t.tensor_type), "offset": int(t.data_offset)}
    if rows is None:
        entry["values"] = [float(v) for v in values.ravel()]
    else:
        entry["rows"] = []
        for r in rows:
            row = values[r] if isinstance(r, int) else values[r[0]][r[1]]
            entry["rows"].append({"index": r, "values": [float(v) for v in row]})
    out["rows"].append(entry)
json.dump(out, open("weights/tests/fixtures/gpt-oss-20b-rows.json", "w"))
print("wrote", sum(len(r.get("rows", [])) for r in out["rows"]), "rows")
