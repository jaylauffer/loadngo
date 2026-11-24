#!/usr/bin/env python3
"""Compare loose Archive CAS objects with a single immutable pack plus fixed index."""

from __future__ import annotations

import argparse
import os
import random
import struct
import sys
import time
from pathlib import Path

INDEX_ENTRY = struct.Struct(">32sQQ")  # BLAKE3 digest, byte offset, byte length
COPY_BYTES = 1024 * 1024


def allocated_bytes(paths: list[Path]) -> int | None:
    stats = [path.stat() for path in paths]
    if any(not hasattr(stat, "st_blocks") for stat in stats):
        return None
    return sum(stat.st_blocks * 512 for stat in stats)


def copy_exact(source, destination, size: int) -> None:
    remaining = size
    while remaining:
        block = source.read(min(COPY_BYTES, remaining))
        if not block:
            raise RuntimeError("source object ended before its recorded size")
        destination.write(block)
        remaining -= len(block)


def compare_streams(left, right, size: int) -> None:
    remaining = size
    while remaining:
        amount = min(COPY_BYTES, remaining)
        a = left.read(amount)
        b = right.read(amount)
        if a != b:
            raise RuntimeError("packed bytes differ from the loose CAS object")
        if not a:
            raise RuntimeError("packed or loose object ended before its recorded size")
        remaining -= len(a)


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Measure a prototype immutable pack containing every raw .blob in an Archive CAS "
            "root, plus a sorted fixed-width BLAKE3/offset/length index. Compare allocated "
            "bytes, sequential reads, and deterministic sampled random reads with loose files. "
            "The input is read-only; pack output is written to a separate directory."
        ),
        epilog=(
            "Example: scripts/repository_pack_layout_benchmark.py --cas-root /Volumes/data/cas "
            "--output /private/tmp/cas-pack-layout"
        ),
    )
    parser.add_argument("--cas-root", required=True, type=Path, help="Archive CAS root containing objects/xx/<hash>.blob")
    parser.add_argument("--output", required=True, type=Path, help="new or empty output directory outside the input CAS")
    parser.add_argument("--random-reads", type=int, default=256, help="number of deterministic per-object reads; default 256")
    parser.add_argument("--seed", type=int, default=20261105, help="sample selection seed; default 20261105")
    args = parser.parse_args()

    source_root = args.cas_root.expanduser().resolve(strict=True)
    output = args.output.expanduser().resolve()
    if args.random_reads < 0:
        parser.error("--random-reads must not be negative")
    if not (source_root / "objects").is_dir():
        parser.error(f"CAS root has no objects directory: {source_root}")
    if source_root == output or source_root in output.parents or output in source_root.parents:
        parser.error("pack output must be separate from, and not contain, the input CAS")
    if output.exists() and any(output.iterdir()):
        parser.error(f"output directory must be empty or absent: {output}")

    objects_root = source_root / "objects"
    objects: list[tuple[bytes, Path, int]] = []
    for path in objects_root.rglob("*.blob"):
        try:
            digest = bytes.fromhex(path.stem)
        except ValueError:
            parser.error(f"object filename is not a hexadecimal digest: {path}")
        if len(digest) != 32:
            parser.error(f"object filename is not a 32-byte digest: {path}")
        objects.append((digest, path, path.stat().st_size))
    objects.sort(key=lambda item: item[0])
    if not objects:
        parser.error(f"no raw .blob objects under {objects_root}")

    output.mkdir(parents=True, exist_ok=True)
    pack_path = output / "objects.pack"
    index_path = output / "objects.idx"
    pack_started = time.monotonic()
    rows: list[tuple[bytes, Path, int, int]] = []
    offset = 0
    with pack_path.open("wb") as pack:
        for digest, path, size in objects:
            rows.append((digest, path, offset, size))
            with path.open("rb") as source:
                copy_exact(source, pack, size)
            offset += size
    pack_seconds = time.monotonic() - pack_started

    with index_path.open("wb") as index:
        index.write(b"LNGOPK01")
        index.write(struct.pack(">Q", len(rows)))
        for digest, _path, row_offset, size in rows:
            index.write(INDEX_ENTRY.pack(digest, row_offset, size))

    # Validate a complete sequential reconstruction against the source object stream.
    sequential_started = time.monotonic()
    with pack_path.open("rb") as packed:
        for _digest, source_path, _offset, size in rows:
            with source_path.open("rb") as source:
                compare_streams(source, packed, size)
        if packed.read(1):
            raise RuntimeError("pack contains trailing bytes not referenced by the index")
    sequential_seconds = time.monotonic() - sequential_started

    sample_count = min(args.random_reads, len(rows))
    sample = random.Random(args.seed).sample(rows, sample_count)
    loose_random_seconds = 0.0
    pack_random_seconds = 0.0
    with pack_path.open("rb") as packed:
        for _digest, source_path, row_offset, size in sample:
            started = time.monotonic()
            with source_path.open("rb") as source:
                loose = source.read()
            loose_random_seconds += time.monotonic() - started
            started = time.monotonic()
            packed.seek(row_offset)
            packed_bytes = packed.read(size)
            pack_random_seconds += time.monotonic() - started
            if loose != packed_bytes:
                raise RuntimeError("sampled packed bytes differ from the loose CAS object")

    loose_paths = [path for _digest, path, _size in objects]
    pack_paths = [pack_path, index_path]
    loose_allocated = allocated_bytes(loose_paths)
    packed_allocated = allocated_bytes(pack_paths)
    logical_bytes = sum(size for _digest, _path, size in objects)
    print(f"objects\t{len(objects)}")
    print(f"logical_object_bytes\t{logical_bytes}")
    print(f"index_entry_bytes\t{INDEX_ENTRY.size}")
    print(f"index_file_bytes\t{index_path.stat().st_size}")
    print(f"pack_file_bytes\t{pack_path.stat().st_size}")
    print(f"loose_allocated_bytes\t{loose_allocated if loose_allocated is not None else 'unavailable'}")
    print(f"packed_allocated_bytes\t{packed_allocated if packed_allocated is not None else 'unavailable'}")
    if loose_allocated is not None and packed_allocated is not None:
        print(f"allocated_bytes_saved\t{loose_allocated - packed_allocated}")
        print(f"allocated_percent_saved\t{(loose_allocated - packed_allocated) / loose_allocated * 100:.3f}")
    print(f"pack_write_seconds\t{pack_seconds:.6f}")
    print(f"sequential_read_verify_seconds\t{sequential_seconds:.6f}")
    print(f"sampled_random_objects\t{sample_count}")
    print(f"loose_sampled_random_read_seconds\t{loose_random_seconds:.6f}")
    print(f"pack_sampled_random_read_seconds\t{pack_random_seconds:.6f}")
    print("byte_verification\tfull sequential comparison and every sampled random object matched")
    print(f"artifacts\t{output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError) as error:
        print(f"repository_pack_layout_benchmark: {error}", file=sys.stderr)
        raise SystemExit(1)
