#!/usr/bin/env python3
"""Opens a loadngo vault file without loadngo.

The way back in if the launcher is ever lost or broken, and the proof that
the file is what docs/VAULT.md says it is: this shares no code with the Rust
that wrote it. The key is derived by the openssl program (3.2 or newer, for
Argon2id); XChaCha20-Poly1305 is written out below from RFC 8439 and the
XChaCha draft, in plain Python.

usage: open-vault.py FILE [--show] [--check] [--master-stdin]

  FILE            required  The vault file (passwords.vault).
  --show          optional  Print the passwords too. Without it: the names,
                            the accounts, and what each password is.
  --check         optional  Also test each password for primality here
                            (Miller-Rabin, 64 random bases) and say so.
  --master-stdin  optional  Read the master password from the first line of
                            standard input, for scripts. Otherwise it is
                            asked for, and what is typed is not shown.

example: open-vault.py ~/Library/Application\\ Support/loadngo-launcher/passwords.vault
"""

import getpass
import json
import random
import struct
import subprocess
import sys

MASK = 0xFFFFFFFF


def rotl(value, count):
    return ((value << count) & MASK) | (value >> (32 - count))


def quarter(state, a, b, c, d):
    state[a] = (state[a] + state[b]) & MASK
    state[d] = rotl(state[d] ^ state[a], 16)
    state[c] = (state[c] + state[d]) & MASK
    state[b] = rotl(state[b] ^ state[c], 12)
    state[a] = (state[a] + state[b]) & MASK
    state[d] = rotl(state[d] ^ state[a], 8)
    state[c] = (state[c] + state[d]) & MASK
    state[b] = rotl(state[b] ^ state[c], 7)


def rounds(state):
    for _ in range(10):
        quarter(state, 0, 4, 8, 12)
        quarter(state, 1, 5, 9, 13)
        quarter(state, 2, 6, 10, 14)
        quarter(state, 3, 7, 11, 15)
        quarter(state, 0, 5, 10, 15)
        quarter(state, 1, 6, 11, 12)
        quarter(state, 2, 7, 8, 13)
        quarter(state, 3, 4, 9, 14)


CONSTANTS = list(struct.unpack("<4I", b"expand 32-byte k"))


def chacha20_block(key, counter, nonce):
    start = CONSTANTS + list(struct.unpack("<8I", key)) + [counter] + list(struct.unpack("<3I", nonce))
    state = list(start)
    rounds(state)
    return struct.pack("<16I", *[(a + b) & MASK for a, b in zip(state, start)])


def hchacha20(key, nonce16):
    state = CONSTANTS + list(struct.unpack("<8I", key)) + list(struct.unpack("<4I", nonce16))
    rounds(state)
    return struct.pack("<8I", *(state[0:4] + state[12:16]))


def chacha20(key, counter, nonce, data):
    out = bytearray()
    for offset in range(0, len(data), 64):
        block = chacha20_block(key, counter + offset // 64, nonce)
        out.extend(a ^ b for a, b in zip(data[offset:offset + 64], block))
    return bytes(out)


def poly1305(key, message):
    r = int.from_bytes(key[:16], "little") & 0x0FFFFFFC0FFFFFFC0FFFFFFC0FFFFFFF
    s = int.from_bytes(key[16:], "little")
    total = 0
    for offset in range(0, len(message), 16):
        chunk = message[offset:offset + 16]
        total = (total + int.from_bytes(chunk + b"\x01", "little")) * r % ((1 << 130) - 5)
    return ((total + s) & ((1 << 128) - 1)).to_bytes(16, "little")


def pad16(data):
    return b"\x00" * (-len(data) % 16)


def xchacha20poly1305_open(key, nonce24, sealed, bound):
    subkey = hchacha20(key, nonce24[:16])
    nonce = b"\x00" * 4 + nonce24[16:]
    body, tag = sealed[:-16], sealed[-16:]
    mac_key = chacha20_block(subkey, 0, nonce)[:32]
    mac_data = bound + pad16(bound) + body + pad16(body) + struct.pack("<QQ", len(bound), len(body))
    expected = poly1305(mac_key, mac_data)
    # Not a constant-time comparison: this is a recovery tool run by the
    # owner on their own file.
    if expected != tag:
        return None
    return chacha20(subkey, 1, nonce, body)


def argon2id(master, salt, memory_kib, passes, lanes):
    out = subprocess.run(
        [
            "openssl", "kdf", "-keylen", "32", "-binary",
            "-kdfopt", "hexpass:" + master.hex(),
            "-kdfopt", "hexsalt:" + salt.hex(),
            "-kdfopt", f"iter:{passes}",
            "-kdfopt", f"memcost:{memory_kib}",
            "-kdfopt", f"lanes:{lanes}",
            "ARGON2ID",
        ],
        capture_output=True,
    )
    if out.returncode != 0 or len(out.stdout) != 32:
        sys.exit("openssl could not derive the key (it needs OpenSSL 3.2 or newer): "
                 + out.stderr.decode(errors="replace").strip())
    return out.stdout


ALPHABETS = [
    ("digits", "0123456789"),
    ("letters and digits", "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"),
    ("letters, digits and signs", "".join(chr(code) for code in range(33, 127))),
]


def number_of(password):
    """The number a password spells, in the smallest alphabet that has all
    its characters, and that alphabet's name; None if it is not a numeral."""
    for name, alphabet in ALPHABETS:
        if password and all(character in alphabet for character in password):
            if alphabet.index(password[0]) == 0:
                return None
            value = 0
            for character in password:
                value = value * len(alphabet) + alphabet.index(character)
            return value, name
    return None


def is_prime(n):
    if n < 2:
        return False
    for p in (2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37):
        if n % p == 0:
            return n == p
    odd, twos = n - 1, 0
    while odd % 2 == 0:
        odd //= 2
        twos += 1
    rng = random.SystemRandom()
    for base in [2] + [rng.randrange(2, n - 1) for _ in range(63)]:
        power = pow(base, odd, n)
        if power in (1, n - 1):
            continue
        for _ in range(twos - 1):
            power = power * power % n
            if power == n - 1:
                break
        else:
            return False
    return True


def main():
    args = sys.argv[1:]
    if not args or "-h" in args or "--help" in args:
        print(__doc__.strip())
        return 0
    flags = {arg for arg in args if arg.startswith("--")}
    unknown = flags - {"--show", "--check", "--master-stdin"}
    paths = [arg for arg in args if not arg.startswith("--")]
    if unknown or len(paths) != 1:
        print("open-vault.py: one file, and --show, --check or --master-stdin; see --help", file=sys.stderr)
        return 2
    with open(paths[0], "rb") as handle:
        data = handle.read()
    if len(data) < 84 + 16 or data[:8] != b"LNGVAULT":
        print("That file is not a vault.", file=sys.stderr)
        return 1
    version, kdf, memory_kib, passes, lanes = struct.unpack("<5I", data[8:28])
    if version != 1 or kdf != 1:
        print(f"A format this tool does not know (version {version}, key derivation {kdf}).", file=sys.stderr)
        return 1
    if "--master-stdin" in flags:
        master = sys.stdin.readline().rstrip("\n")
    else:
        master = getpass.getpass("Master password: ")
    key = argon2id(master.encode(), data[28:60], memory_kib, passes, lanes)
    plain = xchacha20poly1305_open(key, data[60:84], data[84:], data[:60])
    if plain is None:
        print("Wrong master password, or the file is damaged.", file=sys.stderr)
        return 1
    for entry in json.loads(plain)["entries"]:
        line = [entry["name"]]
        if entry.get("account"):
            line.append(entry["account"])
        if entry.get("address"):
            line.append(entry["address"])
        if "--show" in flags:
            line.append(entry["password"])
        if "--check" in flags:
            spelled = number_of(entry["password"])
            if spelled is None:
                line.append("not a number")
            elif is_prime(spelled[0]):
                line.append(f"prime, in {spelled[1]}: {spelled[0]}" if "--show" in flags else f"prime, in {spelled[1]}")
            else:
                line.append("not a prime")
        print("\t".join(line))
    return 0


if __name__ == "__main__":
    sys.exit(main())
