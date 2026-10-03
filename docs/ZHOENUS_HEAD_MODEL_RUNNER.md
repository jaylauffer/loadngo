# Zhoenus head model runner

`network/src/bin/zhoenus_head_model.rs` supervises a local model server for the Zhoenus
talking-head assistant, using `network::model_service`. It starts `llama-server` and
waits for its health check. With `--backend auto` it falls back from Metal to the CPU
when Metal fails to start.

```sh
cargo run --release -p network --bin zhoenus_head_model -- --dry-run
cargo run --release -p network --bin zhoenus_head_model -- --backend metal
```

`--help` lists every option.

## The model comes from the Archive CAS, by content

Until 2026-10-04 the default model was a file path, `~/Downloads/gpt-oss-20b-mxfp4.gguf`.
Any file put at that path would have been served, which made it an easy place to
substitute a model. Jay asked for the model to come from the loadngo CAS instead.

Now the model is named by content: an Archive CAS object, given as a BLAKE3 hash and a
size. No option takes a model path.

- **Default model.** gpt-oss-20b: ggml-org's `MXFP4_MOE` GGUF, Apache 2.0.
  - Hash `56fcc05caeabd1f4f352f7b9d6762cad2035b860973c07a5c330a7fa3944e8e1`, 12,109,565,760
    bytes.
  - Archived in the signed pudding CAS: root `pudding-20260917` `4d8babf2`, entry
    `loadngo/models/gpt-oss-20b-MXFP4_MOE.gguf`.
  - `--model-hash` and `--model-bytes` choose another model.
- **First launch.** The object is restored from a CAS root into
  `~/.loadngo/models/<hash>.gguf`. The root is `--cas-root`, or else the first attached
  root found by `data::cli::discover` that holds the object. The copy is checked
  against the hash while copying (`ArchiveCasStorage::restore_object_to_path`) and then
  made read-only.
- **Every launch.** The cached copy's size and BLAKE3 hash are checked before
  `llama-server` gets its path. A copy that does not match is refused and left in place
  for inspection. This check takes 5.3 s for gpt-oss-20b in a release build. Once
  cached, the CAS drive need not be attached.
- **`--dry-run`.** It says where the model would come from (`cached:` or `cas:`) and
  prints the commands. It copies, hashes and runs nothing.

The 2026-10-04 local copies were deduplicated:

- The `~/Downloads` copy was deleted. It was the same model with a newer chat template
  and 160 slightly different rows of `output.weight`.
- `loadngo/models/gpt-oss-20b-MXFP4_MOE.gguf`, byte-identical to the CAS object, became
  the cache file.

Tests in `model_service`:

- restore once, then serve offline from the verified cache;
- a same-size substituted copy is refused and kept;
- a root without the model, or a directory that is not a CAS, is refused.

Removing the hash comparison fails the substitution test.

## Open

- **`llama-server` is outside loadngo.** It is Homebrew's `llama.cpp`, which Jay does
  not trust, and only the model is verified here, not the server. The intended
  replacement is a Rust engine: a gpt-oss port in kimi-k3-in-rust is outlined in
  `kimi-k3-in-rust/docs/ORCHESTRATION.md`.
- **The default hash is pinned in source,** so trust in it rests on code review. It is
  not yet checked against a signed manifest.
