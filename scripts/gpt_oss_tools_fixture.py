#!/usr/bin/env python3
"""Writes gpt-oss/tests/fixtures/tools-parity.json: conversations with tools rendered by
gpt-oss's own chat template (Jinja2, with transformers' `tojson`, which is plain
`json.dumps`) and tokenized by Hugging Face `tokenizers` with the control tokens as
special tokens -- the reference for gpt-oss/tests/chat_parity.rs.

The declarations have the shape and key order (sorted, as serde_json writes them) of
loadngo_inference's Toolbox::declaration.

    python3 scripts/gpt_oss_tools_fixture.py MODEL.gguf TEMPLATE.jinja
"""
import json
import pathlib
import sys

import jinja2
from gguf import GGUFReader
from tokenizers import AddedToken, Regex, Tokenizer, models, pre_tokenizers

PATTERN = (
    r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]*[\p{Ll}\p{Lm}\p{Lo}\p{M}]+(?i:'s|'t|'re|'ve|'m|'ll|'d)?"
    r"|[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]+[\p{Ll}\p{Lm}\p{Lo}\p{M}]*(?i:'s|'t|'re|'ve|'m|'ll|'d)?"
    r"|\p{N}{1,3}| ?[^\s\p{L}\p{N}]+[\r\n/]*|\s*[\r\n]+|\s+(?!\S)|\s+"
)

reader = GGUFReader(sys.argv[1])
field = reader.fields
def strings(key):
    f = field[key]
    return [bytes(f.parts[i]).decode() for i in f.data]
tokens = strings("tokenizer.ggml.tokens")
types = [int(field["tokenizer.ggml.token_type"].parts[i][0]) for i in field["tokenizer.ggml.token_type"].data]
merges = [tuple(m.split(" ", 1)) for m in strings("tokenizer.ggml.merges")]
vocab = {}
for i, (t, kind) in enumerate(zip(tokens, types)):
    if kind in (1, 3):
        vocab.setdefault(t, i)
tok = Tokenizer(models.BPE(vocab=vocab, merges=merges, ignore_merges=False))
tok.pre_tokenizer = pre_tokenizers.Sequence([
    pre_tokenizers.Split(Regex(PATTERN), behavior="isolated", invert=False),
    pre_tokenizers.ByteLevel(add_prefix_space=False, use_regex=False),
])
tok.add_special_tokens([AddedToken(t, special=True) for t, kind in zip(tokens, types) if kind == 3])

def raise_exception(message):
    raise jinja2.TemplateError(message)
def tojson(x, ensure_ascii=False, indent=None, separators=None, sort_keys=False):
    return json.dumps(x, ensure_ascii=ensure_ascii, indent=indent, separators=separators, sort_keys=sort_keys)
env = jinja2.Environment(trim_blocks=True, lstrip_blocks=True, extensions=["jinja2.ext.loopcontrols"])
env.globals["raise_exception"] = raise_exception
env.globals["strftime_now"] = lambda fmt: "2026-10-04"
env.filters["tojson"] = tojson
template = env.from_string(pathlib.Path(sys.argv[2]).read_text())

def function(name, description, properties, required=None):
    parameters = {"type": "object", "properties": properties}
    if required is not None:
        parameters["required"] = required
    return {"type": "function", "function": {"name": name, "description": description, "parameters": parameters}}

tools = [
    function("fs_read", "Read a text file on the local drive (read-only).", {
        "path": {"type": "string"},
        "line_start": {"type": "integer", "description": "first line, 1-based (default 1)"},
        "line_count": {"type": "integer", "description": "number of lines (default 400)"},
    }, ["path"]),
    function("cas_archives", "List every Archive CAS archive on the attached drives.", {}),
    function("memory_forget", "Drop a note by id.", {"id": {"type": "integer"}}),
]
declaration = json.dumps(tools, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
tools = json.loads(declaration)

cases = [
    {"messages": [{"role": "user", "content": "What is in README.md?"}]},
    {"messages": [{"role": "system", "content": "You are on Jay's Mac mini."},
                  {"role": "user", "content": "Read README.md"},
                  {"role": "assistant", "tool_calls": [{"name": "fs_read", "arguments": {"path": "README.md"}}]},
                  {"role": "tool", "content": "line 1\n\"quoted\" ünïcode\ttab\\back"}],
     "reasoning_effort": "low"},
]
out = []
for case in cases:
    kwargs = {k: v for k, v in case.items() if k != "messages"}
    text = template.render(messages=case["messages"], tools=tools, add_generation_prompt=True, **kwargs)
    out.append({**case, "text": text, "ids": tok.encode(text).ids})
root = pathlib.Path(__file__).resolve().parent.parent
json.dump({"declaration": declaration, "cases": out}, open(root / "gpt-oss/tests/fixtures/tools-parity.json", "w"), ensure_ascii=False, indent=0)
print(len(out), "conversations,", sum(len(c["ids"]) for c in out), "tokens")
print(out[1]["text"][-700:])
