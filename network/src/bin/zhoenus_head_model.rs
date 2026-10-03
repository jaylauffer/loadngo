use anyhow::{bail, Context, Result};
use data::cli::{read_args, ArgDoc, Usage};
use network::model_service::{
    describe_command, locate_model, start_model_server, BackendMode, ModelLocation,
    ModelServerConfig,
};
use std::{path::PathBuf, time::Duration};

const USAGE: Usage = Usage {
    bin: "zhoenus_head_model",
    invocation: "cargo run --release -p network --bin zhoenus_head_model --",
    about: "Serves the Zhoenus talking-head model with llama-server. The model is an \
            Archive CAS object named by its BLAKE3 hash, never a file path: it is restored \
            once into the model cache (checked while copying, then read-only) and hashed \
            again at every launch before llama-server is given it.",
    args: &[
        ArgDoc::optional(
            "--model-hash",
            "HEX",
            "BLAKE3 of the model (default: gpt-oss-20b, 56fcc05c...); needs --model-bytes",
        ),
        ArgDoc::optional("--model-bytes", "N", "the model's size in bytes, with --model-hash"),
        ArgDoc::optional(
            "--cas-root",
            "PATH",
            "Archive CAS root to restore from (default: scan attached drives for one)",
        ),
        ArgDoc::optional("--model-cache", "DIR", "verified copies (default: ~/.loadngo/models)"),
        ArgDoc::optional("--llama-server", "PATH", "llama-server executable (default: llama-server)"),
        ArgDoc::optional("--host", "HOST", "address to serve on (default: 127.0.0.1)"),
        ArgDoc::optional("--port", "N", "port to serve on (default: 8787)"),
        ArgDoc::optional("--backend", "auto|metal|cpu", "compute backend (default: auto)"),
        ArgDoc::optional("--ctx-size", "N", "context length in tokens (default: 4096)"),
        ArgDoc::optional("--threads", "N", "CPU threads (default: llama-server's choice)"),
        ArgDoc::optional(
            "--startup-timeout-seconds",
            "N",
            "how long to wait for the health check (default: 90)",
        ),
        ArgDoc::optional("--health-path", "PATH", "health-check URL path (default: /health)"),
        ArgDoc::repeated("--extra-arg", "ARG", "passed through to llama-server"),
        ArgDoc::switch(
            "--dry-run",
            "say where the model comes from and print the commands; copy and run nothing",
        ),
    ],
    examples: &[
        "cargo run --release -p network --bin zhoenus_head_model -- --dry-run",
        "cargo run --release -p network --bin zhoenus_head_model -- --cas-root '/Volumes/Zhoenus II/pudding-cas' --backend metal",
    ],
    notes: &[
        "The first launch copies the model (12.1 GB for gpt-oss-20b) out of the CAS; later launches hash the copy (5.3 s for gpt-oss-20b in a release build) and need no CAS drive.",
        "A cached copy that does not hash to the model is refused and left in place for inspection.",
    ],
};

#[derive(Debug)]
struct Args {
    config: ModelServerConfig,
    dry_run: bool,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut config = ModelServerConfig::default();
        let mut dry_run = false;
        let mut model_hash = None;
        let mut model_bytes = None;

        let mut args = read_args(&USAGE, false).into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--llama-server" => {
                    config.llama_server =
                        PathBuf::from(args.next().context("missing value for --llama-server")?);
                }
                "--model-hash" => {
                    model_hash = Some(
                        args.next()
                            .context("missing value for --model-hash")?
                            .parse()
                            .context("invalid --model-hash")?,
                    );
                }
                "--model-bytes" => {
                    model_bytes = Some(
                        args.next()
                            .context("missing value for --model-bytes")?
                            .parse()
                            .context("invalid --model-bytes")?,
                    );
                }
                "--cas-root" => {
                    config.cas_root = Some(PathBuf::from(
                        args.next().context("missing value for --cas-root")?,
                    ));
                }
                "--model-cache" => {
                    config.model_cache =
                        PathBuf::from(args.next().context("missing value for --model-cache")?);
                }
                "--host" => {
                    config.host = args.next().context("missing value for --host")?;
                }
                "--port" => {
                    config.port = args
                        .next()
                        .context("missing value for --port")?
                        .parse()
                        .context("invalid --port")?;
                }
                "--backend" => {
                    config.backend =
                        BackendMode::parse(&args.next().context("missing value for --backend")?)?;
                }
                "--ctx-size" => {
                    config.ctx_size = args
                        .next()
                        .context("missing value for --ctx-size")?
                        .parse()
                        .context("invalid --ctx-size")?;
                }
                "--threads" => {
                    config.threads = Some(
                        args.next()
                            .context("missing value for --threads")?
                            .parse()
                            .context("invalid --threads")?,
                    );
                }
                "--startup-timeout-seconds" => {
                    let seconds = args
                        .next()
                        .context("missing value for --startup-timeout-seconds")?
                        .parse()
                        .context("invalid --startup-timeout-seconds")?;
                    config.startup_timeout = Duration::from_secs(seconds);
                }
                "--health-path" => {
                    config.health_path = args.next().context("missing value for --health-path")?;
                }
                "--extra-arg" => {
                    config
                        .extra_args
                        .push(args.next().context("missing value for --extra-arg")?);
                }
                "--dry-run" => {
                    dry_run = true;
                }
                other => bail!("unknown argument: {other}"),
            }
        }
        match (model_hash, model_bytes) {
            (Some(hash), Some(size)) => {
                config.model = data::archive_cas::ArchiveObject { hash, size }
            }
            (None, None) => {}
            _ => bail!("--model-hash and --model-bytes go together"),
        }

        Ok(Self { config, dry_run })
    }
}

fn main() -> Result<()> {
    let args = match Args::parse() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("zhoenus_head_model: {error:#}; {}", USAGE.hint());
            std::process::exit(2);
        }
    };
    args.config.validate()?;

    if args.dry_run {
        let source = match locate_model(&args.config)? {
            ModelLocation::Cached(path) => format!("cached:{}", path.display()),
            ModelLocation::InCas(root) => format!("cas:{}", root.display()),
        };
        let model = args.config.cached_model_path();
        println!(
            "zhoenus_head_model_plan model={} bytes={} source={source} endpoint={} backend={}",
            args.config.model.hash,
            args.config.model.size,
            args.config.endpoint(),
            args.config.backend.label()
        );
        for backend in args.config.attempted_backends() {
            println!(
                "zhoenus_head_model_command backend={} command={}",
                backend.label(),
                describe_command(&args.config, &model, backend)
            );
        }
        return Ok(());
    }

    let server = start_model_server(&args.config)?;
    println!(
        "zhoenus_head_model_ready backend={} endpoint={} pid={}",
        server.backend().label(),
        server.endpoint(),
        server.pid()
    );

    let backend = server.backend();
    let endpoint = server.endpoint().to_string();
    let status = server.wait()?;
    if !status.success() {
        bail!(
            "zhoenus head model service exited unsuccessfully backend={} endpoint={} status={status}",
            backend.label(),
            endpoint
        );
    }
    Ok(())
}
