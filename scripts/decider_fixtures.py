#!/usr/bin/env python3
"""Fixtures for `loadngo-decider`, made with the reference implementation of Strands
Decider (github.com/strands-labs/strands-decider, Apache-2.0) and transformers. The Rust
port is written from the architecture and checked against these; it does not translate
this code.

Run in a Python environment with `strands-decider` installed (its `mlx` extra is not
needed; everything here runs on the CPU in float32):

    python decider_fixtures.py OUT_DIR [--tiny] [--tokenizer] [--prompts] [--e2e]

- `tokenizer-parity.json`: texts, their token ids and character offsets from the
  checkpoint's own tokenizer.
- `prompt-parity.json`: requests rendered and fitted to the window as the reference engine
  does (state ids, question ids, option positions), including truncation.
- `tiny/`: a random Qwen3.5 text model at toy size with a random LoRA adapter and pointer
  head, and transformers' hidden states and head probabilities for it.
- `e2e.json`: the published checkpoint's probabilities for synthetic requests, from the
  reference engine on the CPU in float32.

Every text is synthetic: the repository is public.
"""
import argparse, json, math, pathlib, random, types

CHECKPOINT = "StrandsAgents/strands-decider-2B-hobson-v21"
REVISION = "2b52a6235c1b8306bbfa30b00b9d4b74b63a39f5"

TEXTS = [
    "Help! My payouts have been failing for 3 days!",
    "It's done, they'll see we've won; I'd say you're right. DON'T",
    "Numbers: 12345 and 3.14159, 1,000,000 and -42 and 2026-10-10.",
    "Tabs\tand  double  spaces   and\ttrailing   ",
    "Lines\nand\r\nCRLF\n\n\nand blank lines\n",
    "Accents: café, naïve, résumé, Ångström, façade, Øresund",
    "Decomposed: café and ñ and å",
    "Thai: สวัสดีครับ ห้องนั่งเล่น, Sinhala: ආයුබෝවන්",
    "Chinese: 你好世界，今天天气很好。 Japanese: こんにちは、カタカナ",
    "Emoji 🎉🚀👍🏽 and flags 🇹🇭 and ZWJ 👩‍💻",
    "Symbols — en–dash … «quotes» “curly” ‘single’ © ® ™ € £ ¥",
    "Code: fn main() { let x = vec![1, 2, 3]; println!(\"{x:?}\"); }",
    "Paths ~/pudding/loadngo/target/release/k3 --flag=value",
    "Special-looking text: <think>reasoning</think> and <|im_start|>user and <|endoftext|>",
    "<state>\nA worker reported back.\n</state>\n<question type=\"choice\">\nSelect exactly one option.\nWhich?\n<options>\n1. yes — it is\n2. no\n</options>\n</question>\n<answer>",
    "Mixed nbsp and zero​width and soft­hyphen",
    "ALL CAPS SHOUTING and camelCaseWords and snake_case_words",
    "a" * 300,
    "Combining marks after punctuation: ,́ .̀ !̧",
    "'s 't 're 've 'm 'll 'd 'S 'T 'RE",
    "",
    " ",
    "\n",
]

# Synthetic reports, in the shape the orchestration evaluation asks about.
REPORT = ("A worker agent finished a task in Jay's workspace and reported back.\n\nAgent: Codex\n"
          "Jay's request: Make the archive tool refuse a substituted model file.\n\nThe worker's report:\n"
          "The model service now names its model by BLAKE3 hash and checks the cached copy on every "
          "launch; a same-size substituted copy is refused and kept for inspection. Gates: fmt, clippy "
          "-D warnings and 41 tests pass on macOS. Open: not run on Linux; CI not yet seen.")
STATE_Q = ("What state is the work in? Judge the report itself: a claim is verified only if the report "
           "shows the check that ran. Whether Jay must act is a separate question.")
STATES = {
    "verified": "verified: done, and the report names checks that ran and passed",
    "unverified": "not verified: done or mostly done, but a check did not run or its result is not known yet (CI pending, not run on a device, not played, not reviewed, claims without evidence)",
    "failed": "failed: the work did not do what was asked, or a check failed",
}
ACT_Q = ("Does Jay need to look at this or act on it: decide, approve, push, sign, test by hand, or "
         "deal with a problem in the work?")
LONG = " ".join(
    f"Paragraph {i}: the build ran on the machine, the tests passed, and the logs were kept in the "
    f"archive under a dated folder; nothing was pushed and the release waits for review." for i in range(40))


def requests():
    """Synthetic requests in TypeSafe's shape."""
    return [
        {"name": "readme", "state": "Help! My payouts have been failing for 3 days! ",
         "questions": {
             "choice_0": {"type": "choice", "instructions": "Which team should handle this?",
                          "criteria": {"billing": None, "sales": None, "retail": None}},
             "noul_0": {"type": "noul", "instructions": "Does this convey urgency?"},
             "score_0": {"type": "score", "instructions": "How frustrated is the writer?",
                         "criteria": ["calm", "frustrated", "depressed"]}}},
        {"name": "report", "state": REPORT,
         "questions": {
             "state": {"type": "choice", "instructions": STATE_Q, "criteria": STATES},
             "attention": {"type": "noul", "instructions": ACT_Q}}},
        {"name": "single-noul", "state": "The release was signed and uploaded; the store shows version 0.5.17.",
         "questions": {"done": {"type": "noul", "instructions": "The release is published."}}},
        {"name": "unicode", "state": "ห้องนั่งเล่น 🎉 café naïve — “quoted” and café decomposed.",
         "questions": {
             "lang": {"type": "choice", "instructions": "Which languages appear?",
                      "criteria": {"thai": "Thai script", "french": "French words", "chinese": "Chinese characters",
                                   "none": "", "all": "every one of these"}}}},
        {"name": "long-score", "state": LONG,
         "questions": {
             "risk": {"type": "score", "instructions": "How risky is it to delete these logs?",
                      "criteria": ["no risk", "minor inconvenience", "real loss", "irreplaceable", "catastrophic"]},
             "pushed": {"type": "noul", "instructions": "The work was pushed."}}},
    ]


def truncation_requests():
    """Requests whose prompts do not fit the window (ids only; too long for an e2e run)."""
    huge = " ".join(f"word{i}" for i in range(6000))
    long_q = "Consider the following at length. " * 2000
    return [
        {"name": "long-state", "state": huge,
         "questions": {"q": {"type": "noul", "instructions": "It is long."}}},
        {"name": "long-question", "state": "short state",
         "questions": {"q": {"type": "choice", "instructions": long_q,
                             "criteria": {"a": "first", "b": "second"}},
                       "r": {"type": "noul", "instructions": "It is short."}}},
        {"name": "many-questions", "state": LONG,
         "questions": {f"q{i}": {"type": "noul", "instructions": f"Statement number {i} holds."}
                       for i in range(40)}},
    ]


def checkpoint():
    from huggingface_hub import snapshot_download
    return snapshot_download(CHECKPOINT, revision=REVISION)


def annotated(r):
    """`r` with each choice's option order written out: JSON readers that sort object keys
    (serde_json without `preserve_order`) would otherwise lose it."""
    r = json.loads(json.dumps(r))
    for q in r["questions"].values():
        if q["type"] == "choice":
            q["criteria_order"] = list(q["criteria"])
    return r


def to_request(r):
    from strands_decider.schema import SystemOneRequest
    return SystemOneRequest(state=r["state"], questions=r["questions"])


def fitted(engine_like, request):
    """The prompt the reference engine forwards, chunk by chunk: state ids, question ids,
    suffix-relative option positions, kinds."""
    from strands_decider.prompting import render_question, render_state
    rendered = [render_question(q) for q in request.questions.values()]
    out = []
    for chunk, s, q, offsets in engine_like._chunks(render_state(request.state), rendered):
        engine_like._last_offsets = offsets
        rq = rendered[chunk]
        idx = engine_like._option_idx(rq, 0).tolist()
        out.append({"state_ids": s, "question_ids": q,
                    "option_index": [[i for i in row if i >= 0] for row in idx],
                    "kinds": [r.kind for r in rq], "labels": [list(r.slot_labels) for r in rq]})
    return out


def stub_engine(tok, max_length=4096):
    from strands_decider.infer import EngineConfig, SystemOneEngine
    e = SystemOneEngine.__new__(SystemOneEngine)
    e.tok = tok
    e.cfg = EngineConfig(device="cpu")
    e.model = types.SimpleNamespace(config=types.SimpleNamespace(max_length=max_length, head_type="pointer"))
    e.device = "cpu"
    return e


def tokenizer_fixture(out):
    from transformers import AutoTokenizer
    tok = AutoTokenizer.from_pretrained(checkpoint())
    cases = []
    for t in TEXTS:
        enc = tok(t, add_special_tokens=True, return_offsets_mapping=True)
        cases.append({"text": t, "ids": enc["input_ids"], "offsets": [list(o) for o in enc["offset_mapping"]]})
    (out / "tokenizer-parity.json").write_text(json.dumps(cases, ensure_ascii=False, indent=0) + "\n")
    print("tokenizer:", len(cases), "texts,", sum(len(c["ids"]) for c in cases), "tokens")


def prompt_fixture(out):
    from transformers import AutoTokenizer
    tok = AutoTokenizer.from_pretrained(checkpoint())
    engine = stub_engine(tok)
    cases = []
    for r in requests() + truncation_requests():
        cases.append({"request": annotated(r), "order": list(r["questions"]), "chunks": fitted(engine, to_request(r))})
    (out / "prompt-parity.json").write_text(json.dumps(cases, ensure_ascii=False) + "\n")
    print("prompts:", len(cases), "requests")


def e2e_fixture(out):
    import torch
    from strands_decider.infer import load_engine
    torch.manual_seed(0)
    engine = load_engine(checkpoint(), device="cpu")
    cfg = engine.model.config
    cases = []
    for r in requests():
        request = to_request(r)
        response = engine.evaluate(request)
        cases.append({"request": annotated(r), "order": list(r["questions"]),
                      "answers": json.loads(response.model_dump_json())["answers"],
                      "chunks": fitted(engine, request)})
        print("e2e:", r["name"], {k: v for k, v in cases[-1]["answers"].items()})
    (out / "e2e.json").write_text(json.dumps({
        "checkpoint": f"{CHECKPOINT}@{REVISION}",
        "temperature": cfg.temperature, "temperature_by_kind": cfg.temperature_by_kind,
        "cases": cases}, ensure_ascii=False, indent=0) + "\n")


def tiny_fixture(out):
    """A random Qwen3.5 text model at toy size (three Gated DeltaNet layers, one gated
    full-attention layer, twice), a random LoRA adapter on every projection the decider
    adapts, and a random pointer head."""
    import torch
    from peft import LoraConfig, get_peft_model
    from safetensors.torch import save_file
    from transformers import Qwen3_5TextConfig
    from transformers.models.qwen3_5.modeling_qwen3_5 import Qwen3_5TextModel

    torch.manual_seed(20261010)
    lin, full = "linear_attention", "full_attention"
    cfg = Qwen3_5TextConfig(
        vocab_size=256, hidden_size=64, intermediate_size=96, num_hidden_layers=8,
        layer_types=[lin, lin, lin, full] * 2, num_attention_heads=4, num_key_value_heads=2,
        head_dim=16, linear_num_key_heads=2, linear_num_value_heads=4, linear_key_head_dim=8,
        linear_value_head_dim=8, linear_conv_kernel_dim=4, rms_norm_eps=1e-6,
        rope_parameters={"rope_type": "default", "rope_theta": 10000.0, "partial_rotary_factor": 0.5,
                         "mrope_section": [2, 1, 1], "mrope_interleaved": True},
        tie_word_embeddings=True)
    cfg._attn_implementation = "eager"
    model = Qwen3_5TextModel(cfg).float().eval()
    with torch.no_grad():
        for name, p in model.named_parameters():
            if name.endswith("A_log"):
                p.copy_(torch.log(torch.empty_like(p).uniform_(0.5, 8.0)))
            elif name.endswith("dt_bias"):
                p.copy_(torch.empty_like(p).uniform_(-1.0, 1.0))
            elif "norm" in name:
                p.copy_(torch.randn_like(p) * 0.2 + (1.0 if name.endswith("linear_attn.norm.weight") else 0.0))
            else:
                p.copy_(torch.randn_like(p) * 0.15)
    tiny = out / "tiny"
    tiny.mkdir(parents=True, exist_ok=True)
    save_file({f"model.language_model.{k}": v.contiguous() for k, v in model.state_dict().items()},
              str(tiny / "model.safetensors"))
    text_cfg = json.loads(cfg.to_json_string())
    (tiny / "config.json").write_text(json.dumps({"model_type": "qwen3_5", "text_config": text_cfg}, indent=1) + "\n")

    targets = ["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj",
               "in_proj_qkv", "in_proj_z", "in_proj_a", "in_proj_b", "out_proj"]
    peft_model = get_peft_model(model, LoraConfig(r=4, lora_alpha=8, lora_dropout=0.0, target_modules=targets))
    with torch.no_grad():
        for name, p in peft_model.named_parameters():
            if "lora_" in name:
                p.copy_(torch.randn_like(p) * 0.1)
    peft_model.eval()
    peft_model.save_pretrained(str(tiny / "lora"))

    dim = 16
    head = {"norm.weight": torch.randn(64) * 0.2 + 1.0, "norm.bias": torch.randn(64) * 0.1,
            "q.weight": torch.randn(dim, 64) * 0.2, "q.bias": torch.randn(dim) * 0.1,
            "k.weight": torch.randn(dim, 64) * 0.2, "k.bias": torch.randn(dim) * 0.1}
    save_file({k: v.contiguous() for k, v in head.items()}, str(tiny / "head.safetensors"))
    (tiny / "strands_decider_config.json").write_text(json.dumps({
        "base_model": "tiny", "head_type": "pointer", "pointer_dim": dim, "max_length": 96,
        "temperature": 0.9, "temperature_by_kind": {"noul": 0.8, "choice": 0.8, "score": 1.2},
        "use_lora": True, "lora_r": 4, "lora_alpha": 8, "torch_dtype": "float32"}, indent=1) + "\n")

    ids = [random.Random(7).randrange(256) for _ in range(70)]
    with torch.no_grad():
        hidden = peft_model(input_ids=torch.tensor([ids]), attention_mask=torch.ones(1, len(ids))).last_hidden_state[0]
        pooled = hidden[-1]
        options = hidden[[20, 41, 63]]
        ln = torch.nn.functional.layer_norm
        q = ln(pooled, (64,), head["norm.weight"], head["norm.bias"]) @ head["q.weight"].T + head["q.bias"]
        k = ln(options, (64,), head["norm.weight"], head["norm.bias"]) @ head["k.weight"].T + head["k.bias"]
        logits = (k @ q) * dim ** -0.5
    (tiny / "hidden.f32").write_bytes(hidden.numpy().astype("<f4").tobytes())
    (tiny / "oracle.json").write_text(json.dumps({
        "ids": ids, "options": [20, 41, 63], "head_logits": logits.tolist(),
        "probabilities_at_0.8": torch.softmax(logits / 0.8, -1).tolist()}, indent=1) + "\n")
    print("tiny: 8 layers, 70 positions")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    for flag in ("tiny", "tokenizer", "prompts", "e2e"):
        ap.add_argument(f"--{flag}", action="store_true")
    a = ap.parse_args()
    out = pathlib.Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    every = not (a.tiny or a.tokenizer or a.prompts or a.e2e)
    if a.tokenizer or every:
        tokenizer_fixture(out)
    if a.prompts or every:
        prompt_fixture(out)
    if a.tiny or every:
        tiny_fixture(out)
    if a.e2e or every:
        e2e_fixture(out)


if __name__ == "__main__":
    main()
