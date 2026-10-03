#!/usr/bin/env python3
"""Writes gpt-oss/tests/fixtures/tokenizer-parity.json: texts and the tokens Hugging
Face `tokenizers` gives them, with the vocabulary and merges of gpt-oss-20b's GGUF and
the o200k split pattern (a Split pre-tokenizer, then ByteLevel without its own regex) --
the reference for gpt-oss/tests/tokenizer_parity.rs.

    python3 scripts/gpt_oss_tokenizer_fixture.py ~/.loadngo/models/56fcc05c...gguf
"""
import json
import pathlib
import sys

from gguf import GGUFReader
from tokenizers import Regex, Tokenizer, decoders, models, pre_tokenizers

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
    if kind == 1:
        vocab.setdefault(t, i)
tok = Tokenizer(models.BPE(vocab=vocab, merges=merges, ignore_merges=False))
tok.pre_tokenizer = pre_tokenizers.Sequence([
    pre_tokenizers.Split(Regex(PATTERN), behavior="isolated", invert=False),
    pre_tokenizers.ByteLevel(add_prefix_space=False, use_regex=False),
])
tok.decoder = decoders.ByteLevel()

root = pathlib.Path(__file__).resolve().parent.parent
texts = [
    "The capital of France is",
    "Hello, world! It's 2026-10-04 and the time is 04:15.",
    "I'M SURE they'll say THEY'VE seen it; we'd've.",
    "Straße, naïve café, résumé — “quotes” and ‘apostrophes’.",
    "北京是中国的首都。我会说一点普通话。",
    "日本語のテキストとカタカナ、ひらがな。",
    "한국어 문장입니다.",
    "Здравствуйте, мир! Ελληνικά κείμενα.",
    "مرحبا بالعالم",
    "emoji 🙂👍🏽 and flags 🇹🇭 with ZWJ 👩‍💻",
    "combining: é ä ñ, and a lone mark ́x",
    "numbers 1234567 3.14159 1e-9 0x1F 1,000,000",
    "tabs\tand\t\ttabs\n\n\nlines\r\nwindows\r\n",
    "    indented code\n        deeper\n",
    "trailing spaces   \nand   \n\n  ",
    "path/to/file.rs:42:7 and http://example.com/a?b=c&d=e",
    "x'S y'ſ z'RE Q'Ve",
    "fn main() {\n    println!(\"{}\", 1 + 2);\n}\n",
    "<|start|>user<|message|>Hello<|end|> spelled as text",
    " no-break em　ideographic space",
    "",
    " ",
    "a",
]
for name in ["proactor/src/lib.rs", "README.md", "weights/src/gguf.rs"]:
    texts.append((root / name).read_text()[:6000])
cases = [{"text": t, "ids": tok.encode(t).ids} for t in texts]
assert all(tok.decode(c["ids"]) == c["text"] for c in cases)
out = root / "gpt-oss/tests/fixtures/tokenizer-parity.json"
json.dump(cases, open(out, "w"), ensure_ascii=False)
print(len(cases), "texts,", sum(len(c["ids"]) for c in cases), "tokens")
