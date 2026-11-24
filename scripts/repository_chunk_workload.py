#!/usr/bin/env python3
"""Run the repository chunk probe across every tracked regular working-tree file."""

from __future__ import annotations

import argparse
import collections
import os
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path

try:
    import resource
except ImportError:  # resource is unavailable on Windows.
    resource = None


def probe_output(
    path: Path,
    cas_root: Path,
    whole_cas_root: Path,
    hybrid_cas_root: Path,
    probe: Path,
    temp: Path,
    index: int,
):
    stem = f"file-{index:08}"
    command = [
        str(probe),
        "--cas-root",
        str(cas_root),
        "--whole-cas-root",
        str(whole_cas_root),
        "--hybrid-cas-root",
        str(hybrid_cas_root),
        "--input",
        str(path),
        "--manifest",
        str(temp / f"{stem}.manifest"),
        "--output",
        str(temp / f"{stem}.chunked"),
        "--whole-file-output",
        str(temp / f"{stem}.whole"),
    ]
    outputs = command[command.index("--manifest") + 1::2]
    try:
        result = subprocess.run(command, check=False, capture_output=True, text=True)
        if result.returncode:
            raise RuntimeError(
                f"probe failed for {path} (exit {result.returncode}):\n"
                f"{result.stderr.strip()}\n{result.stdout.strip()}"
            )
        fields = {}
        for line in result.stdout.splitlines():
            key, separator, value = line.partition("\t")
            if separator:
                fields[key] = value
        return fields
    finally:
        for output in outputs:
            Path(output).unlink(missing_ok=True)


def regular_tracked_files(root: Path, skipped: collections.Counter):
    listing = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"],
        check=True,
        capture_output=True,
    ).stdout
    for raw_path in listing.split(b"\0"):
        if not raw_path:
            continue
        relative = Path(os.fsdecode(raw_path))
        candidate = root / relative
        try:
            mode = candidate.lstat().st_mode
        except FileNotFoundError:
            skipped["missing"] += 1
            continue
        if os.path.islink(candidate):
            skipped["symlink"] += 1
            continue
        if not stat.S_ISREG(mode):
            skipped["non_regular"] += 1
            continue
        yield relative, candidate


def cas_storage(root: Path):
    stats = [path.stat() for path in root.rglob("*") if path.is_file()]
    file_bytes = sum(entry.st_size for entry in stats)
    if any(not hasattr(entry, "st_blocks") for entry in stats):
        allocated_bytes = "unavailable"
    else:
        allocated_bytes = sum(entry.st_blocks * 512 for entry in stats)
    return len(stats), file_bytes, allocated_bytes


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Measure pure CDC, whole-file, and production v3 hybrid CAS storage, "
            "reconstruction latency, and per-extension totals across a tracked worktree."
        ),
        epilog=(
            "Example: scripts/repository_chunk_workload.py --repo-root . "
            "--probe target/release/repository_chunk_probe --cas-root /Volumes/data/repo-probe-cas"
        ),
    )
    parser.add_argument("--repo-root", required=True, type=Path, help="Git working tree to scan")
    parser.add_argument("--probe", required=True, type=Path, help="built repository_chunk_probe executable")
    parser.add_argument("--cas-root", required=True, type=Path, help="new or empty parent directory for separate chunk and whole-file CAS roots")
    args = parser.parse_args()

    root = args.repo_root.resolve(strict=True)
    probe = args.probe.resolve(strict=True)
    cas_parent = args.cas_root.expanduser().resolve()
    chunk_cas = cas_parent / "chunked"
    whole_cas = cas_parent / "whole-file"
    hybrid_cas = cas_parent / "hybrid-v3"
    if not os.access(probe, os.X_OK):
        parser.error(f"probe is not executable: {probe}")
    if not os.path.isdir(root):
        parser.error(f"repository root is not a directory: {root}")
    git_root = Path(
        subprocess.check_output(
            ["git", "-C", str(root), "rev-parse", "--show-toplevel"], text=True
        ).strip()
    ).resolve()
    if git_root != root:
        parser.error(f"--repo-root must be the Git worktree root: {git_root}")
    roots = (chunk_cas, whole_cas, hybrid_cas)
    if cas_parent == root or len(set(roots)) != len(roots) or any(path == root for path in roots):
        parser.error("CAS roots must be separate from each other and the input root")
    if root == cas_parent or root in cas_parent.parents:
        parser.error("CAS root must be outside the input working tree")
    if chunk_cas.exists() and any(chunk_cas.iterdir()):
        parser.error(f"chunk CAS directory must be empty or absent: {chunk_cas}")
    if whole_cas.exists() and any(whole_cas.iterdir()):
        parser.error(f"whole-file CAS directory must be empty or absent: {whole_cas}")
    if hybrid_cas.exists() and any(hybrid_cas.iterdir()):
        parser.error(f"hybrid CAS directory must be empty or absent: {hybrid_cas}")
    chunk_cas.mkdir(parents=True, exist_ok=True)
    whole_cas.mkdir(parents=True, exist_ok=True)
    hybrid_cas.mkdir(parents=True, exist_ok=True)

    before = resource.getrusage(resource.RUSAGE_CHILDREN) if resource else None
    started = time.monotonic()
    totals = collections.defaultdict(collections.Counter)
    thermal = collections.Counter()
    skipped = collections.Counter()
    hybrid_threshold = None
    processed = 0
    with tempfile.TemporaryDirectory(prefix="loadngo-chunk-workload-") as temp_dir:
        temp = Path(temp_dir)
        for relative, path in regular_tracked_files(root, skipped):
            fields = probe_output(
                path, chunk_cas, whole_cas, hybrid_cas, probe, temp, processed
            )
            size = int(fields["input_bytes"])
            threshold = int(fields["hybrid_small_file_whole_threshold_bytes"])
            if hybrid_threshold is not None and hybrid_threshold != threshold:
                raise RuntimeError("probe reported inconsistent hybrid thresholds")
            hybrid_threshold = threshold
            category = path.suffix.lower() or "[no_extension]"
            row = totals[category]
            row["files"] += 1
            row["logical_bytes"] += size
            for field, key in (
                ("chunks", "chunk_refs"),
                ("unique_chunks_this_run", "new_chunk_objects"),
                ("new_chunk_bytes", "new_chunk_bytes"),
                ("manifest_bytes", "manifest_bytes"),
                ("chunk_ingest_ms", "chunk_ingest_ms"),
                ("chunk_restore_ms", "chunk_restore_ms"),
                ("whole_file_ingest_ms", "whole_file_ingest_ms"),
                ("whole_file_restore_ms", "whole_file_restore_ms"),
                ("hybrid_objects_added", "hybrid_objects_added"),
                ("hybrid_object_bytes_added", "hybrid_object_bytes_added"),
                ("hybrid_ingest_ms", "hybrid_ingest_ms"),
            ):
                row[key] += int(fields[field])
            row["new_manifest_objects"] += fields["manifest_inserted"] == "true"
            if fields["manifest_inserted"] == "true":
                row["new_manifest_bytes"] += int(fields["manifest_object_bytes"])
            row["new_whole_file_objects"] += fields["whole_file_inserted"] == "true"
            row["new_whole_file_bytes"] += int(fields["whole_file_cas_bytes_added"])
            row["hybrid_whole_files"] += fields["hybrid_strategy"] == "whole-file"
            row["hybrid_chunked_files"] += fields["hybrid_strategy"] == "chunked"
            thermal[fields["thermal_pressure"]] += 1
            processed += 1
    after = resource.getrusage(resource.RUSAGE_CHILDREN) if resource else None
    elapsed = time.monotonic() - started
    max_rss = "unavailable"
    child_user = child_system = "unavailable"
    if before and after:
        max_rss = after.ru_maxrss
        if sys.platform == "darwin":
            max_rss //= 1024
        child_user = f"{after.ru_utime - before.ru_utime:.3f}"
        child_system = f"{after.ru_stime - before.ru_stime:.3f}"

    print(f"profile\tloadngo-buzhash64-cdc-v2")
    print(f"tracked_regular_files\t{processed}")
    print(f"wall_seconds\t{elapsed:.3f}")
    print(f"child_user_cpu_seconds\t{child_user}")
    print(f"child_system_cpu_seconds\t{child_system}")
    print(f"child_peak_rss_kib\t{max_rss}")
    for name, path in (("chunk", chunk_cas), ("whole_file", whole_cas), ("hybrid_v3", hybrid_cas)):
        object_files, file_bytes, allocated_bytes = cas_storage(path)
        print(f"{name}_cas_object_files\t{object_files}")
        print(f"{name}_cas_file_bytes\t{file_bytes}")
        print(f"{name}_cas_allocated_bytes\t{allocated_bytes}")
    print(f"hybrid_whole_file_threshold_bytes\t{hybrid_threshold}")
    print(f"probe_final_thermal_pressure_counts\t{dict(sorted(thermal.items()))}")
    columns = (
        "files", "logical_bytes", "chunk_refs", "new_chunk_objects", "new_chunk_bytes",
        "new_manifest_objects", "manifest_bytes", "new_manifest_bytes",
        "new_whole_file_objects", "new_whole_file_bytes", "chunk_ingest_ms",
        "chunk_restore_ms", "whole_file_ingest_ms", "whole_file_restore_ms",
        "hybrid_whole_files", "hybrid_chunked_files", "hybrid_objects_added",
        "hybrid_object_bytes_added", "hybrid_ingest_ms",
    )
    print("extension\t" + "\t".join(columns))
    grand_total = collections.Counter()
    for category, row in sorted(totals.items()):
        grand_total.update(row)
        print(
            f"{category}\t{row['files']}\t{row['logical_bytes']}\t{row['chunk_refs']}\t"
            f"{row['new_chunk_objects']}\t{row['new_chunk_bytes']}\t{row['new_manifest_objects']}\t"
            f"{row['manifest_bytes']}\t{row['new_manifest_bytes']}\t{row['new_whole_file_objects']}\t"
            f"{row['new_whole_file_bytes']}\t{row['chunk_ingest_ms']}\t{row['chunk_restore_ms']}\t"
            f"{row['whole_file_ingest_ms']}\t{row['whole_file_restore_ms']}\t"
            f"{row['hybrid_whole_files']}\t{row['hybrid_chunked_files']}\t"
            f"{row['hybrid_objects_added']}\t{row['hybrid_object_bytes_added']}\t"
            f"{row['hybrid_ingest_ms']}"
        )
    print("[TOTAL]\t" + "\t".join(str(grand_total[column]) for column in columns))
    if skipped:
        print(f"skipped\t{dict(sorted(skipped.items()))}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, subprocess.SubprocessError, RuntimeError, KeyError, ValueError) as error:
        print(f"repository_chunk_workload: {error}", file=sys.stderr)
        raise SystemExit(1)
