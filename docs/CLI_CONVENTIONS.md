# Command-line conventions for loadngo tools and harnesses

Every standalone loadngo binary -- the Archive CAS family
(`archive_cas_ingest`, `archive_cas_verify`, `archive_cas_sign`,
`archive_cas_gc`, `archive_cas_exclude`, `archive_cas_remove`,
`archive_cas_restore`, `archive_cas_prune_manifests`,
`archive_cas_browser`), `pudding_cas_ingest`/`pudding_cas_verify`, and every
test harness under `*/src/bin/` -- is run by hand, from memory, often months
apart, by whichever agent or person is on call. There is no shell history to
lean on and, for most of these, no user other than another agent or Jay
running it directly from a terminal. The tool itself has to be the
documentation.

## The rule

1. Running a tool with `--help` or `-h` always prints a full description:
   what the tool does, every flag it accepts (with a one-line description
   and whether it's required), at least one worked example, and any notes a
   reader needs before running it (dry-run defaults, what "success" means,
   safety caveats). This goes to **stdout**, exit code 0 -- it is
   documentation, not an error.
2. Running a tool with **no arguments at all** does the same, *if and only
   if* the tool has at least one required argument. An empty invocation of
   such a tool cannot succeed anyway, so showing the docs on the first try
   beats a bare `missing --foo` with nothing else to go on. A tool that is
   fully usable with zero arguments (every flag has a working default, or
   it's an interactive launcher -- see below) must not treat an empty argv
   as a request for help; that would break its normal use.
3. Any other parse failure -- an unknown flag, a missing required one --
   prints one short line to **stderr** and a reminder to pass `--help`,
   rather than repeating the whole usage block or silently guessing at what
   was meant.

This mirrors ordinary Unix practice (`git`, `cargo`, `kubectl`): help is
documentation and goes to stdout; a real error goes to stderr and stays
short. Whichever of these a binary gets wrong is exactly the complaint that
prompted this doc -- CAS tools that had a `--help`, but a one-line dense
`Usage:` string with no per-argument explanation, and no story for what
happens when you run them with nothing.

## Using the shared helper (`data::cli`)

Any binary in a crate that already depends on `data` (which is most of
them -- `data` is a workspace-wide grab bag, not just Archive CAS) should
use `data::cli::{ArgDoc, Usage}` rather than hand-rolling `eprintln!`
usage strings. See `data/src/cli.rs` for the full API; the shape is:

```rust
use data::cli::{ArgDoc, Usage};

fn usage() -> Usage {
    // `const` bindings here, not a bare `&[...]` literal passed straight
    // into the struct -- Rust's rvalue-static-promotion does not reliably
    // reach through a `const fn` call nested that deep, and `Usage::args`
    // needs a `'static` slice.
    const ARGS: &[ArgDoc] = &[
        ArgDoc::required("--source", "<directory>", "what this flag does"),
        ArgDoc::optional("--label", "<text>", "defaults to <something>"),
        ArgDoc::repeated("--path", "<relative-path>", "may be passed more than once"),
        ArgDoc::switch("--execute", "actually do it; without it, dry-run only"),
    ];
    const EXAMPLES: &[&str] = &["cargo run -p data --bin my_tool -- --source ..."];
    const NOTES: &[&str] = &["Dry-run by default.", "See docs/WHATEVER.md for the model this checks."];
    Usage {
        bin: "my_tool",
        invocation: "cargo run -p data --bin my_tool --",
        about: "one or two sentences: what this does and does not do",
        args: ARGS,
        examples: EXAMPLES,
        notes: NOTES,
    }
}

impl Args {
    fn parse() -> anyhow::Result<Self> {
        // `true` here means "no arguments at all also shows help" -- pass
        // `false` for a tool where every flag has a default, or a launcher
        // that treats an empty argv as its own valid mode (see below).
        let mut args = data::cli::read_args(&usage(), true).into_iter();
        // ... existing hand-rolled `while let Some(arg) = args.next()` loop,
        // unchanged, except every `missing`/`unknown argument` error should
        // append `\n{}` with `usage().hint()` so the error itself points
        // back at `--help`.
        todo!()
    }
}
```

`data::cli::read_args` handles `--help`/`-h` and the empty-argv case before
your own parsing loop runs, and returns the remaining arguments (program
name already stripped) either way. `Usage::hint()` gives the one-line
"run `<invocation> --help` for the full option list" reminder to append to
a parse error with `anyhow!("...\n{}", usage().hint())`.

A binary in a crate that cannot depend on `data` (or shouldn't, to keep its
own dependency footprint small -- `network`, `pq-auth`, and
`proactor-harness`'s harnesses are all like this today) should still follow
rules 1-3 above; it just writes its own small `print_usage()` and
`--help`/empty-argv check inline, in the same shape `data`'s binaries used
before this convention existed. Consistent behavior matters more than a
shared dependency.

## Discovering a CAS root, instead of requiring one up front

A tool that *operates on a specific already-known CAS root* --
`archive_cas_ingest`, `archive_cas_verify`, `archive_cas_sign`,
`archive_cas_gc`, and the rest of the one-shot Archive CAS tools -- should
keep requiring `--cas-root` explicitly. There is no reasonable default
answer to "which CAS root did you mean" when the tool is about to write,
delete, or make an authoritative claim about one specific archive.

`archive_cas_browser`, the one binary in this family that *launches* rather
than performing a single operation and exiting, is different: it is
read-only, it can show more than one CAS root at once (each labeled with the
device it's mounted from), and "let me see what's out there" is itself a
useful thing to ask for. So when it is launched with **no** `--cas-root` at
all, it scans attached storage for anything that looks like a CAS root and
opens every one it finds, printing what it found first. See
`data::cli::discover` for the scan (mount-point enumeration per platform,
bounded-depth recursive search for a directory with both an `objects/` and
a `manifests/` subdirectory, skipping OS bookkeeping directories like
`System Volume Information` or `.Trashes`) and
[`ARCHIVE_CAS_BROWSER.md`](ARCHIVE_CAS_BROWSER.md) for the user-facing
behavior. If the scan finds nothing, the browser explains where it looked
and exits without opening an empty window, rather than a bare
"`--cas-root` is required".

This is deliberately not a new interactive picker UI: the browser already
renders one device-labeled banner row per CAS root side by side, built for
the `--cas-root <a> --cas-root <b>` case. Reusing it for "found on this
scan" needed no new widget.

If a future tool needs the same "point me at a root, or help me find one"
shape, reuse `data::cli::discover` rather than writing a second scanner. A
tool where the discovered root will be *written to* (not just browsed)
should still ask for confirmation before proceeding -- printing the
candidate and requiring `--cas-root <path>` to actually act on it is enough
for that; a destructive tool must never guess.

**Watch for mount-point aliases.** macOS always has a symlink alias like
`/Volumes/Macintosh HD -> /` sitting next to real volume mounts, and
`/Volumes` itself is a real (non-symlink) directory reachable through it --
so a naive scan that follows that symlink loops straight back into every
other real mount and rediscovers the same CAS root a second time under a
bogus path, which showed up as a duplicated device banner in the browser.
`candidate_mount_points()` filters out symlinked entries for exactly this
reason, and `scan_for_cas_roots()`/the browser's `--cas-root` list both run
through `dedup_by_canonical_path` as a second line of defense against any
other alias mechanism (a relative-vs-absolute spelling, a different
symlink). `cargo test -p data --lib -- --ignored --nocapture
print_real_candidate_mount_points` prints what discovery actually sees on
the machine it runs on, without opening the browser's window -- run it
after touching anything in `data::cli::discover` before trusting the
result.

## What "all tools and harnesses" covers today

The Archive CAS and pudding CAS binaries (`data/src/bin/*.rs`) and
`archive_cas_browser` (`host-desktop/src/bin/archive_cas_browser.rs`) follow
this convention as of 2026-09-20. The remaining binaries under
`*/src/bin/` across the workspace (the `host-desktop` test harnesses,
`network`'s `task_*` tools, `pq-auth/src/bin/loadngo_pq_auth.rs`, and
`proactor-harness`'s `profile-target`/`stress`) have not all been brought in
line with it yet -- bring a binary's help up to this standard as part of
the next real change that touches it, rather than as a separate sweep with
no functional motivation.
