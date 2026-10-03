#!/usr/bin/env python3
"""Writes gpt-oss/tests/fixtures/chat-parity.json: conversations rendered by gpt-oss's
own chat template (Jinja2, as transformers renders it) and tokenized by Hugging Face
`tokenizers` with the control tokens as special tokens -- the reference for
gpt-oss/tests/chat_parity.rs.

    python3 scripts/gpt_oss_chat_fixture.py MODEL.gguf TEMPLATE.jinja
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
env = jinja2.Environment(trim_blocks=True, lstrip_blocks=True, extensions=["jinja2.ext.loopcontrols"])
env.globals["raise_exception"] = raise_exception
env.globals["strftime_now"] = lambda fmt: "2026-10-04"
template = env.from_string(pathlib.Path(sys.argv[2]).read_text())

cases = [
    {"messages": [{"role": "user", "content": "What is the capital of France?"}]},
    {"messages": [{"role": "system", "content": "Answer in one word."},
                  {"role": "user", "content": "Capital of Japan?"},
                  {"role": "assistant", "content": "Tokyo", "thinking": "dropped"},
                  {"role": "user", "content": "And Thailand?"}],
     "reasoning_effort": "low"},
    {"messages": [{"role": "user", "content": "多语言：你好 🙂\n\n  indented"}], "reasoning_effort": "high",
     "model_identity": "You are Kimi, running locally on a Mac mini."},
]
out = []
for case in cases:
    kwargs = {k: v for k, v in case.items() if k != "messages"}
    text = template.render(messages=case["messages"], add_generation_prompt=True, **kwargs)
    out.append({**case, "text": text, "ids": tok.encode(text).ids})
root = pathlib.Path(__file__).resolve().parent.parent
json.dump(out, open(root / "gpt-oss/tests/fixtures/chat-parity.json", "w"), ensure_ascii=False, indent=0)
print(len(out), "conversations,", sum(len(c["ids"]) for c in out), "tokens")
print(out[1]["text"])
