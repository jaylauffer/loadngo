//! `decide`: Strands Decider answering System One requests on this machine's CPU.

use std::{io::Read, path::PathBuf, time::Instant};

use loadngo_decider::Decider;
use loadngo_inference::system_one::{response_json, Request};

const USAGE: &str = "\
decide: answer typed System One questions with Strands Decider (Qwen3.5-2B and a
pointer head), on the CPU, with nothing generated.

Reads a request from stdin and prints the answers:

  {\"state\": \"...\", \"questions\": {\"id\": {\"type\": \"noul\" | \"choice\" | \"score\",
   \"instructions\": \"...\", \"criteria\": {\"label\": \"description\", ...}}}}
  -> {\"answers\": {\"id\": {\"label\": probability, ...}}}

Options (all optional):
  --checkpoint DIR   the decider checkpoint (strands_decider_config.json, tokenizer.json,
                     head.safetensors, lora/); default: the Hugging Face cache's
                     StrandsAgents/strands-decider-2B-hobson-v21 at 2b52a623
  --base DIR         its base model, Qwen3.5-2B-Base (config.json, safetensors); default:
                     the Hugging Face cache's Qwen/Qwen3.5-2B-Base at b1485b2f
  --eval FILE        instead of a request, score the orchestration cases in FILE
                     (loadngo_inference::agent::eval): report to stderr, JSON to stdout
  -h, --help         this text

Example:
  echo '{\"state\": \"Help! My payouts have been failing for 3 days!\",
         \"questions\": {\"urgent\": {\"type\": \"noul\",
                         \"instructions\": \"Does this convey urgency?\"}}}' | decide
";

fn fail(message: &str) -> ! {
    eprintln!("decide: {message}; run with --help for the options");
    std::process::exit(2);
}

fn hf_cache(repo: &str, revision: &str) -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_else(|| fail("HOME is not set"));
    PathBuf::from(home)
        .join(".cache/huggingface/hub")
        .join(format!("models--{}", repo.replace('/', "--")))
        .join("snapshots")
        .join(revision)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut checkpoint = None;
    let mut base = None;
    let mut eval = None;
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .map(PathBuf::from)
                .unwrap_or_else(|| fail(&format!("{arg} needs a value")))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            "--checkpoint" => checkpoint = Some(value()),
            "--base" => base = Some(value()),
            "--eval" => eval = Some(value()),
            other => fail(&format!("unknown option {other}")),
        }
    }
    let checkpoint = checkpoint.unwrap_or_else(|| {
        hf_cache(
            "StrandsAgents/strands-decider-2B-hobson-v21",
            "2b52a6235c1b8306bbfa30b00b9d4b74b63a39f5",
        )
    });
    let base = base.unwrap_or_else(|| {
        hf_cache(
            "Qwen/Qwen3.5-2B-Base",
            "b1485b2fa6dfa1287294f269f5fb618e03d52d7c",
        )
    });
    let started = Instant::now();
    let mut decider = Decider::load(&checkpoint, &base).unwrap_or_else(|e| fail(&e.to_string()));
    eprintln!("decide: loaded in {:.1}s", started.elapsed().as_secs_f64());
    if let Some(path) = eval {
        evaluate(&mut decider, &path);
        return;
    }
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .unwrap_or_else(|e| fail(&format!("reading stdin: {e}")));
    let request = Request::from_text(&text).unwrap_or_else(|e| fail(&e));
    let started = Instant::now();
    let answers = decider
        .answer(&request)
        .unwrap_or_else(|e| fail(&e.to_string()));
    eprintln!(
        "decide: answered in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    println!("{}", response_json(&answers));
}

fn evaluate(decider: &mut Decider, path: &std::path::Path) {
    use loadngo_inference::agent::eval;
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| fail(&format!("{}: {e}", path.display())));
    let cases = eval::load(&text).unwrap_or_else(|e| fail(&e));
    let mut run = eval::Run::default();
    for (i, case) in cases.iter().enumerate() {
        let started = Instant::now();
        let j = eval::judge(decider, case).unwrap_or_else(|e| fail(&e));
        eprintln!(
            "[{}/{}] {}: key {}, model {} (attention {:.2}), rules {}; {:.1}s",
            i + 1,
            cases.len(),
            case.id,
            case.class.name(),
            j.best().name(),
            j.attention,
            eval::mechanical(&case.report).name(),
            started.elapsed().as_secs_f64()
        );
        run.add(case, Some(&j));
    }
    eprint!("{}", run.report("strands-decider"));
    println!("{}", run.to_json("strands-decider"));
}
