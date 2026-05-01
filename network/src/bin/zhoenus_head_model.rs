use anyhow::{bail, Context, Result};
use network::model_service::{
    describe_command, model_file_size, start_model_server, BackendMode, ModelServerConfig,
};
use std::{env, path::PathBuf, time::Duration};

#[derive(Debug)]
struct Args {
    config: ModelServerConfig,
    dry_run: bool,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut config = ModelServerConfig::default();
        let mut dry_run = false;

        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--llama-server" => {
                    config.llama_server =
                        PathBuf::from(args.next().context("missing value for --llama-server")?);
                }
                "--model-path" => {
                    config.model_path =
                        PathBuf::from(args.next().context("missing value for --model-path")?);
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
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other}"),
            }
        }

        Ok(Self { config, dry_run })
    }
}

fn main() -> Result<()> {
    let args = Args::parse()?;
    args.config.validate()?;

    if args.dry_run {
        let model_size = model_file_size(&args.config.model_path)?;
        println!(
            "zhoenus_head_model_plan model={} bytes={} endpoint={} backend={}",
            args.config.model_path.display(),
            model_size,
            args.config.endpoint(),
            args.config.backend.label()
        );
        for backend in args.config.attempted_backends() {
            println!(
                "zhoenus_head_model_command backend={} command={}",
                backend.label(),
                describe_command(&args.config, backend)
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

fn print_usage() {
    eprintln!(
        "usage: cargo run -p network --bin zhoenus_head_model -- \
         [--model-path /Users/jay/Downloads/gpt-oss-20b-mxfp4.gguf] \
         [--llama-server llama-server] [--host 127.0.0.1] [--port 8787] \
         [--backend auto|metal|cpu] [--ctx-size 4096] [--threads n] \
         [--startup-timeout-seconds 90] [--health-path /health] \
         [--extra-arg <arg>] [--dry-run]"
    );
}
