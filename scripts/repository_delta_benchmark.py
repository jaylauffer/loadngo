#!/usr/bin/env python3
"""Measure an ML-DSA-signed Git import and changed-file CAS transfer end to end."""

from __future__ import annotations

import argparse
import random
import re
import subprocess
import sys
import time
from pathlib import Path

try:
    import resource
except ImportError:  # resource is unavailable on Windows.
    resource = None


def command(args: list[str], *, stdout=subprocess.PIPE):
    started = time.monotonic()
    result = subprocess.run(args, stdout=stdout, stderr=subprocess.PIPE, check=False)
    elapsed = time.monotonic() - started
    if result.returncode:
        detail = result.stderr.decode(errors="replace").strip()
        raise RuntimeError(f"command failed ({result.returncode}): {args[0]} {args[1:]}\n{detail}")
    output = result.stdout.decode(errors="replace") if stdout == subprocess.PIPE else ""
    return output, elapsed


def parse_import_version(output: str) -> str:
    found = re.search(r" at ([0-9a-f]{64}) \(git ", output)
    if not found:
        raise RuntimeError(f"could not parse imported version from: {output.strip()}")
    return found.group(1)


def parse_send(output: str) -> tuple[int, int]:
    found = re.search(r": (\d+) versions, (\d+) objects, (\d+) bytes\.", output)
    if not found:
        raise RuntimeError(f"could not parse send report from: {output.strip()}")
    return int(found.group(2)), int(found.group(3))


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Create a two-commit Git fixture with a deterministic binary asset, import and sign "
            "both versions, send the baseline and incremental update, then verify and byte-check checkout."
        ),
        epilog=(
            "Example: scripts/repository_delta_benchmark.py --repo-cli target/release/loadngo-repo "
            "--keygen-cli target/release/loadngo_pq_auth --work-root /Volumes/data/pq-delta-bench "
            "--edit-mode middle-insert"
        ),
    )
    parser.add_argument("--repo-cli", required=True, type=Path, help="built loadngo-repo executable")
    parser.add_argument("--keygen-cli", required=True, type=Path, help="built loadngo_pq_auth executable")
    parser.add_argument("--work-root", required=True, type=Path, help="new or empty output directory outside a Git checkout")
    parser.add_argument("--file-bytes", type=int, default=2 * 1024 * 1024, help="baseline random asset size; default 2097152")
    parser.add_argument(
        "--edit-mode",
        choices=("prefix-insert", "middle-insert", "append", "overwrite"),
        default="prefix-insert",
        help="single edit applied to the binary asset; default prefix-insert",
    )
    parser.add_argument("--prefix-byte", type=lambda value: int(value, 0), default=0xFF, help="byte inserted at the start of the asset; default 0xff")
    args = parser.parse_args()

    repo_cli = args.repo_cli.expanduser().resolve(strict=True)
    keygen_cli = args.keygen_cli.expanduser().resolve(strict=True)
    root = args.work_root.expanduser().resolve()
    if args.file_bytes <= 0:
        parser.error("--file-bytes must be positive")
    if not 0 <= args.prefix_byte <= 255:
        parser.error("--prefix-byte must be in 0..255")
    if not repo_cli.is_file() or not keygen_cli.is_file():
        parser.error("CLI paths must name executable files")
    if not repo_cli.stat().st_mode & 0o111 or not keygen_cli.stat().st_mode & 0o111:
        parser.error("CLI paths must be executable")
    if root.exists() and any(root.iterdir()):
        parser.error(f"work root must be empty or absent: {root}")
    if any((parent / ".git").exists() for parent in (root, *root.parents)):
        parser.error("work root must be outside any Git checkout")
    root.mkdir(parents=True, exist_ok=True)
    child_usage_before = resource.getrusage(resource.RUSAGE_CHILDREN) if resource else None
    benchmark_started = time.monotonic()

    source = root / "git-source"
    (source / "assets").mkdir(parents=True)
    original = random.Random(20261105).randbytes(args.file_bytes)
    large_file = source / "assets" / "large.bin"
    large_file.write_bytes(original)
    (source / "README.md").write_text("PQ repository transfer benchmark\n", encoding="utf-8")
    command(["git", "init", "--quiet", "--initial-branch=main", str(source)])
    for key, value in (("user.name", "Loadngo Benchmark"), ("user.email", "benchmark@localhost")):
        command(["git", "-C", str(source), "config", key, value])
    command(["git", "-C", str(source), "add", "-A"])
    command(["git", "-C", str(source), "commit", "--quiet", "-m", "baseline"])
    command(["git", "-C", str(source), "branch", "baseline"])
    edit_offset = 0
    if args.edit_mode == "prefix-insert":
        changed = bytes([args.prefix_byte]) + original
    elif args.edit_mode == "middle-insert":
        edit_offset = len(original) // 2
        changed = original[:edit_offset] + bytes([args.prefix_byte]) + original[edit_offset:]
    elif args.edit_mode == "append":
        edit_offset = len(original)
        changed = original + bytes([args.prefix_byte])
    else:
        edit_offset = len(original) // 2
        replacement = args.prefix_byte
        if replacement == original[edit_offset]:
            replacement ^= 0xFF
        changed = original[:edit_offset] + bytes([replacement]) + original[edit_offset + 1:]
    large_file.write_bytes(changed)
    command(["git", "-C", str(source), "add", "assets/large.bin"])
    command(["git", "-C", str(source), "commit", "--quiet", "-m", "prefix byte"])

    public_key = root / "trusted.pub"
    private_key = root / "signer.key"
    key_output, _ = command([
        str(keygen_cli), "keygen", "--public-key", str(public_key),
        "--private-key", str(private_key),
    ])
    if "ml-dsa-65" not in key_output:
        raise RuntimeError(f"keygen did not report ML-DSA-65: {key_output.strip()}")

    source_store = root / "source-store"
    receiver_store = root / "receiver-store"
    signing = [
        "--trusted-public-key", str(public_key),
        "--signer-identity", "benchmark",
        "--public-key", str(public_key),
        "--private-key", str(private_key),
    ]
    common = [
        "--store", str(source_store), "--repository", "pq-delta", "--branch", "main",
        "--from-git", str(source),
    ]
    base_output, base_import_seconds = command([
        str(repo_cli), "import", *common, "--git-ref", "baseline", *signing,
    ])
    base_version = parse_import_version(base_output)

    def send_to_receiver():
        return command([
            str(repo_cli), "send", "--store", str(source_store),
            "--repository", "pq-delta", "--branch", "main", "--to", str(receiver_store),
            "--trusted-public-key", str(public_key), "--remote-trusted-public-key", str(public_key),
        ])

    initial_output, initial_send_seconds = send_to_receiver()
    base_objects, base_payload_bytes = parse_send(initial_output)
    tip_output, incremental_import_seconds = command([
        str(repo_cli), "import", *common, "--git-ref", "main", *signing,
    ])
    tip_version = parse_import_version(tip_output)
    delta_output, incremental_send_seconds = send_to_receiver()
    delta_objects, delta_payload_bytes = parse_send(delta_output)

    stream_path = root / "incremental.stream"
    with stream_path.open("wb") as stream:
        _, stream_seconds = command([
            str(repo_cli), "send", "--store", str(source_store),
            "--repository", "pq-delta", "--branch", "main",
            "--trusted-public-key", str(public_key), "--stream-stdout",
            "--known-version", base_version,
        ], stdout=stream)
    framed_bytes = stream_path.stat().st_size

    verify_output, verify_seconds = command([
        str(repo_cli), "verify", "--store", str(receiver_store),
        "--repository", "pq-delta", "--branch", "main",
        "--trusted-public-key", str(public_key),
    ])
    checkout = root / "receiver-checkout"
    checkout_output, checkout_seconds = command([
        str(repo_cli), "checkout", "--store", str(receiver_store),
        "--repository", "pq-delta", "--branch", "main",
        "--destination", str(checkout), "--trusted-public-key", str(public_key),
    ])
    tracked = subprocess.check_output(["git", "-C", str(source), "ls-files", "-z"]).split(b"\0")
    compared = 0
    for item in tracked:
        if not item:
            continue
        relative = Path(item.decode())
        if (source / relative).read_bytes() != (checkout / relative).read_bytes():
            raise RuntimeError(f"receiver checkout differs from Git at {relative}")
        compared += 1

    private_key.unlink()
    changed_file_bytes = large_file.stat().st_size
    child_usage_after = resource.getrusage(resource.RUSAGE_CHILDREN) if resource else None
    child_peak_rss_kib = "unavailable"
    child_user_cpu = child_system_cpu = "unavailable"
    voluntary_switches = involuntary_switches = "unavailable"
    if child_usage_before and child_usage_after:
        child_peak_rss_kib = child_usage_after.ru_maxrss
        if sys.platform == "darwin":
            child_peak_rss_kib //= 1024
        child_user_cpu = f"{child_usage_after.ru_utime - child_usage_before.ru_utime:.6f}"
        child_system_cpu = f"{child_usage_after.ru_stime - child_usage_before.ru_stime:.6f}"
        voluntary_switches = child_usage_after.ru_nvcsw - child_usage_before.ru_nvcsw
        involuntary_switches = child_usage_after.ru_nivcsw - child_usage_before.ru_nivcsw
    benchmark_elapsed = time.monotonic() - benchmark_started
    print("signature_scheme\tml-dsa-65")
    print(f"edit_mode\t{args.edit_mode}")
    print(f"edit_offset\t{edit_offset}")
    print(f"baseline_file_bytes\t{args.file_bytes}")
    print(f"changed_file_bytes\t{changed_file_bytes}")
    print(f"changed_file_fraction_of_delta_payload_percent\t{delta_payload_bytes / changed_file_bytes * 100:.3f}")
    print(f"delta_payload_saving_percent\t{(1 - delta_payload_bytes / changed_file_bytes) * 100:.3f}")
    print(f"delta_stream_framed_bytes\t{framed_bytes}")
    print(f"framed_stream_fraction_of_changed_file_percent\t{framed_bytes / changed_file_bytes * 100:.3f}")
    print(f"baseline_send_objects\t{base_objects}")
    print(f"baseline_send_payload_bytes\t{base_payload_bytes}")
    print(f"delta_send_objects\t{delta_objects}")
    print(f"delta_send_payload_bytes\t{delta_payload_bytes}")
    print(f"baseline_import_seconds\t{base_import_seconds:.3f}")
    print(f"incremental_import_seconds\t{incremental_import_seconds:.3f}")
    print(f"initial_send_seconds\t{initial_send_seconds:.3f}")
    print(f"incremental_send_seconds\t{incremental_send_seconds:.3f}")
    print(f"stream_seconds\t{stream_seconds:.3f}")
    print(f"verify_seconds\t{verify_seconds:.3f}")
    print(f"checkout_seconds\t{checkout_seconds:.3f}")
    print(f"benchmark_wall_seconds\t{benchmark_elapsed:.3f}")
    print(f"child_user_cpu_seconds\t{child_user_cpu}")
    print(f"child_system_cpu_seconds\t{child_system_cpu}")
    print(f"child_peak_rss_kib\t{child_peak_rss_kib}")
    print(f"child_voluntary_context_switches\t{voluntary_switches}")
    print(f"child_involuntary_context_switches\t{involuntary_switches}")
    print(f"byte_compared_files\t{compared}")
    print(f"baseline_version\t{base_version}")
    print(f"updated_version\t{tip_version}")
    print(f"verification\t{verify_output.strip()}")
    print(f"checkout\t{checkout_output.strip()}")
    print(f"artifacts\t{root}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, subprocess.SubprocessError, RuntimeError, ValueError) as error:
        print(f"repository_delta_benchmark: {error}", file=sys.stderr)
        raise SystemExit(1)
