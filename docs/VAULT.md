# Vault

The loadngo launcher is also a password vault: it makes passwords and keeps
them (Jay, 2026-11-24: "the loadngo launcher needs to also be a vault,
password generator and storage of passwords, each password should be thought
of as a prime number, a true algorithm").

- The crate `vault/` (`loadngo-vault`) makes the passwords and seals the
  file. It has no window.
- The launcher's **Vault** button (`launcher/src/vault.rs`) is the window on
  it, on a computer. `launcher --vault` opens on it.

## Each password is a prime number

This is meant literally. A password is read as a numeral: each character is a
digit, worth its place in the alphabet, and the leftmost counts most.

| Alphabet | Base | Characters, in order of worth |
|---|---|---|
| Digits | 10 | `0` to `9` |
| Letters and digits | 62 | `0-9`, `A-Z`, `a-z` |
| Letters, digits and signs | 94 | every character a keyboard prints but the space, `!` to `~` |

So `7Kp1`, in letters and digits, is 7 × 62³ + 20 × 62² + 51 × 62 + 1 =
1,748,339, which is prime. Every password the vault makes is a prime in this
sense, and the launcher shows the number beside a password that is shown.

### How one is made (`vault/src/generate.rs`)

1. Draw a numeral of the wanted length from the operating system's random
   source, one character at a time, each character of the alphabet as likely
   as another. A random byte that would favour some characters is thrown
   away and another drawn. The first character is never the alphabet's zero,
   as a number's numeral does not begin with zero.
2. If it has not at least one character of each kind in the alphabet (a
   digit, a capital, a small letter, and for the largest alphabet a sign),
   throw it away. This is for the sites that ask for "a capital, a digit and
   a sign", and it makes a password read back in the alphabet it was made
   in.
3. Test the number for primality. If it is not prime, throw it away and go
   back to 1.

Because every numeral is drawn afresh and independently, each prime the
recipe allows is as likely as any other: nothing carries over from one
password to the next, and the search does not favour primes that follow a
long gap (as "add two until prime" would). About one numeral in
`length × ln(base)` is prime, so the default recipe looks at about a hundred
numerals.

### The primality test (`vault/src/prime.rs`)

1. Division by every prime below 2048. A number below 2048² that none of
   them divides is prime, and that is a proof.
2. Miller-Rabin. Write n − 1 as d × 2ˢ with d odd. For a prime n and any
   base a, either aᵈ = 1, or a^(d × 2ʳ) = −1 for some r below s, modulo n,
   because the only square roots of one modulo a prime are one and minus
   one. A composite n fails this for at least three bases in four.
   - Below 2⁶⁴ the twelve bases 2, 3, …, 37 catch every composite, so the
     answer is a proof.
   - Above, base 2 and then 63 bases drawn at random. A composite passes all
     64 with probability below 4⁻⁶⁴ = 2⁻¹²⁸. **That is a bound, not a
     proof.** It is the test FIPS 186-5 prescribes for the primes of
     signature keys.

The arithmetic is loadngo's own (`vault/src/number.rs`): whole numbers of up
to 512 bits, and multiplication and powers modulo an odd number by
Montgomery's method. A password of 64 characters in the largest alphabet is
420 bits.

### What a prime buys

- **Each stands alone.** Two different primes share no factor. Nothing about
  one password says anything about another.
- **It can be checked, by anyone, without being told where it came from.**
  The vault never records "made here": it reads every password it holds as a
  number and tests it. The green dot and "A prime number" are the result of
  that test, on this machine, when the vault was unlocked or the entry
  saved.
- **A slip usually shows.** A password typed in by hand and wrong in one
  character is prime about one time in a hundred, so "Not a prime number"
  under a password that should be one is a warning.

### What it costs

Primes are scarcer than numerals, and an attacker who knows the recipe knows
to try only primes. The strength the launcher shows allows for that: it is
the base-2 logarithm of how many passwords the recipe can make, by the prime
number theorem and the share with every kind of character.

| Recipe | All numerals | What the recipe can make |
|---|---|---|
| 23 of letters, digits and signs (the default) | 150.8 bits | about 144 bits |
| 23 of letters and digits | 136.9 bits | about 130 bits |
| 8 of letters and digits (the shortest) | 47.6 bits | about 42 bits |
| 6 digits | 19.9 bits | about 16 bits (68,906 primes) |

Lengths: 4 to 64 digits, 8 to 64 of the other two.

## The file

Everything is in one file, `passwords.vault`, in the launcher's data folder
beside `catalog.json` (macOS `~/Library/Application Support/loadngo-launcher`,
Linux `~/.local/share/loadngo-launcher`, Windows `%APPDATA%\loadngo-launcher`).

```text
 0   8  "LNGVAULT"
 8   4  format version, 1                 (integers little-endian)
12   4  key derivation, 1 = Argon2id version 1.3
16   4  Argon2id memory, in KiB           (65536)
20   4  Argon2id passes                   (3)
24   4  Argon2id lanes                    (1)
28  32  salt
60  24  nonce
84   .  the entries as JSON, sealed with XChaCha20-Poly1305, then its
        16-byte tag; the 60 bytes before the nonce are bound in as
        associated data
```

- **The key** is Argon2id(master password as typed, in UTF-8; salt), 32
  bytes, at the costs the file names: 64 MiB and three passes, 64 to 86 ms
  on the Mac mini (M4 Pro). A guess at the master password costs an attacker
  the same. A file may ask for no more than 1 GiB and 16 passes, so
  a file from elsewhere cannot make the launcher take all the memory there
  is.
- **Every write draws a new nonce**; the salt and so the key stay for the
  life of the file.
- **Nothing in it is readable without the key**: not a name, not an account.
  Its length gives away roughly how much it holds.
- **The master password is kept nowhere.** If it is lost, what is in the
  vault is lost with it. There is no back door and no recovery.
- The sealed JSON is `{"entries":[{"name","account","address","password",
  "changed"}]}`, `changed` being when the password was last set, in seconds
  since 1970.

Writing (`Sealed::write`): the new file is checked to open, to the same
entries, before anything is written; it is written beside the old one as
`passwords.vault.new` (readable by its owner alone), forced to disk and read
back; only then does it take the old one's name, in one step, so the name
holds a whole vault at every moment. The file it replaces is kept as
`passwords.vault.before`. A write is refused when the file on disk is not the
one the vault last read or wrote, so two launchers open on one vault cannot
silently undo each other: the second is told, and must lock and unlock to
read the file again.

**Back the file up.** One file, and it is useless without the master
password, so it can go wherever backups go.

### The primitives are not loadngo's own

Argon2id and XChaCha20-Poly1305 come from the RustCrypto crates `argon2` 0.5
and `chacha20poly1305` 0.10, pure Rust, no C. This is the one place the
vault prefers someone else's code: these are the two functions where a
subtle mistake gives away everything silently, and theirs have had far more
eyes than a new one would. The prime numbers, which are the part the request
is about, are loadngo's.

What holds them to the standards:

- `vault/src/store.rs` derives the reference implementation's own known
  answer (password `password`, salt `somesalt`, two passes, 64 MiB), which
  OpenSSL 3.6 gives too.
- `vault/tools/open-vault.py` opens a vault file with no loadngo code at
  all: Argon2id by the `openssl` program, and XChaCha20-Poly1305 written out
  from RFC 8439 in plain Python. It is also the way back in if the launcher
  is ever lost. `open-vault.py --help` says how.

## The window

**Vault**, in the launcher's header, gives the window to the vault; **List**
(or Escape) gives it back. The vault stays as it was, locked or not.

- **The first time** it asks for a master password twice (at least 8
  characters) and makes the file.
- **Locked**, it asks for the master password. A wrong one and a damaged file
  cannot be told apart, and are not.
- **Unlocked**, it lists what it keeps, by name. Each entry has a dot (green:
  its password is a prime number; grey: it is not; red: another entry has
  the same password) and **Copy**. The pane at the bottom is the selected
  entry: **Show** (the password, and the number it spells), **Change**,
  **Delete** (which asks once more), **Copy account**.
- **New** and **Change** open a form: name, account, address, password. The
  password may be typed or pasted (one kept from elsewhere), or made:
  choose the alphabet and the length, and **Make a prime**. The line under
  the field says what the password in it is.

| Key | |
|---|---|
| Up, Down | Move the selection |
| Enter, Command-C | Copy the selected entry's password |
| Space | Show the selected entry's password, and hide it |
| Command-N | A new entry; in the form, a new prime |
| Tab, Shift-Tab | The next field, the one before |
| Enter, Command-S | In the form: save |
| Command-V | Paste into the field |
| Command-L | Lock |
| Escape | Leave the form unsaved; from the list, back to the launcher's list |

Control does what Command does, for Linux and Windows.

Two things happen by the clock, and are the only reason the launcher's
window ever wakes without input:

- **A copied password is taken off the clipboard 43 seconds later**, if it is
  still what the clipboard holds. Something copied since is left alone. A
  launcher quit sooner takes it off on the way out.
- **The vault locks itself after 307 seconds** with nothing pressed or typed
  in it, whichever screen is up. Locking drops the entries and the key, which
  wipes them; a form left open goes with it, unsaved. It does not lock over
  a change that could not be written.

## What it does not defend against

- **Someone at the unlocked vault**, or malware running as the same user
  while it is unlocked: both can read what the vault can.
- **A keylogger**, which sees the master password typed.
- **The clipboard.** For 43 seconds a copied password is where any program
  can read it, and a clipboard manager may keep it for good. The launcher
  does not mark it as a secret for such managers yet.
- **Memory.** Passwords, numbers and the key are wiped when dropped, and the
  fields here keep no undo history. But a password that has been drawn on
  screen has passed through the text renderer's own buffers, and nothing
  stops the system writing memory to swap or to a crash report.
- **Timing.** The primality arithmetic does not run in constant time. It
  runs on the machine that made the number, in the program that will show
  it, so there is nobody new to leak to; it would matter if this code ever
  tested a secret for someone else.
- **A weak master password.** Argon2id makes each guess cost 64 MiB and
  some hundredths of a second; it does not make a short password long.
- **Being killed.** A launcher ended by a signal or a crash within 43
  seconds of a copy leaves the password on the clipboard.

## Checked

2026-11-24, on the Mac mini (M4 Pro, macOS 26). Pictures of the real window
are in `vault/evidence/2026-11-24/`; the vault in them is a scratch one, made
for the check and since discarded.

Unit tests. `cargo test -p loadngo-vault` (38, and one measurement run by
hand):

- The arithmetic against Python's (`pow(3, 200, 2**255 - 19)`, a division,
  2²⁵⁶ in decimal), against plain 128-bit arithmetic on small moduli, and at
  full width (Fermat's test on 2⁵¹² − 569, the largest prime that fits).
- The primality test against trial division for every number below 10,000;
  eight known primes from 23 to 512 bits; composites chosen to fool weaker
  tests: Carmichael numbers, a number that passes Miller-Rabin to the first
  nine primes, one above 2⁶⁴ that passes to all twelve fixed bases (so only
  the random bases catch it), a product of two large primes and a prime
  squared. Every one of these numbers was checked in Python first.
- The generator: what it makes is a prime of the length with every kind of
  character, in each alphabet at four lengths, and sixty times running at the
  shortest; 300 six-digit passwords are prime by division, a test sharing no
  code with the maker; over 63,660 draws each of the 1,061 four-digit primes
  was made, none far from its share (chi-squared); bytes that would favour
  some characters are never used; a random source stuck on one byte ends in
  an error, not a loop.
- The file: what goes in comes out with the master password and not
  without; nothing kept is readable in the file; a changed bit in any byte of
  the header, or in the first or last sealed byte, is refused; a file asking
  for 2 GiB of memory is refused at once; each write keeps the file before
  it; a second vault open on the same file cannot write over the first's
  change; the key is Argon2id by the reference implementation's known
  answer.

`cargo test -p loadngo-launcher` (61, of which 27 are new): the field (typing,
the caret, a hidden field's text in nothing drawn), the Vault button (and
none on a phone), and the vault's screens through made-up input: making a
vault, a wrong and a right master password, a new entry given a prime and
found on disk, a password in nothing drawn until shown, copy and its 43
seconds, locking after 307, delete asking twice, two entries with one
password, a write that fails, lock waiting for a write, the keys.

Eighteen breaks were made on purpose, one at a time (a numeral not tested
for primality, the unfair bytes used, no random bases, a write over a file
changed by another, a file readable by others, a vault that never locks, a
password always drawn, a hidden field drawing its text, a phone given the
vault, and so on). Seventeen failed a test, two of them only after the test
was sharpened. The eighteenth was a check that another check repeats.

The real launcher, built `--release`, with a scratch folder
(`LAUNCHER_STATE_DIR`), driven by keys posted to its process and pictures of
its window:

- The first-run screen; a master password typed twice; the file made (114
  bytes, mode 600).
- A new entry with a made password: the form said "A prime number", the list
  showed it with a green dot, Show showed 23 characters and a 46-digit
  number.
- **`open-vault.py` opened that file**, read the entry, and found the
  password prime by its own Miller-Rabin; the number it computed is the one
  the launcher showed. A wrong master password was refused.
- Copy put the 23 characters on the system clipboard; 43 seconds later, with
  no input, the clipboard was empty, once with the vault's screen up and
  once with the launcher's list up.
- A wrong master password on the locked screen was refused in words;
  Command-L locked.
- Quit (Command-Q) 2 seconds after a copy: the clipboard was empty
  afterwards. (Found by this check: the first build left it there.)
- Left unlocked and untouched for 312 seconds: the window was on the locked
  screen.
- Argon2id at the standard costs: 64 to 86 ms
  (`cargo test -p loadngo-vault --release -- --ignored --nocapture standard_costs`).

Idle. The vault's two clocks are the launcher's first use of a frame "no
later than", so the question is whether an unlocked vault costs anything
while it waits. The same process, 30 seconds each, untouched (`top`):

| | Context switches | Idle wake-ups | Processor time |
|---|---|---|---|
| Vault locked, window in front (no clock running) | 803 | 0 | 0.02 s |
| Vault unlocked, window in front (the lock's clock running) | 1,030 | 0 | 0.02 s |
| Vault unlocked, a copy's clock running too, in front | 1,368 | 0 | 0.08 s |
| The launcher's list, window no longer on screen | 110 | 6 | 0.03 s |
| The installed launcher (an earlier build), in the background, same time | 57 | 0 | 0.00 s |

Locked and unlocked are alike, so the clocks themselves cost nothing that
shows. **Not explained:** a launcher window in front made 27 to 46 context
switches a second, a launcher out of sight 2 to 4. The Mac was in use at the
time, and a pointer moving over a window brings it frames, which is the
likely cause; that was not established, and nor was whether the figure is
new since the 22 in 30 seconds recorded on 2026-11-16.

Other systems. `cargo check` of the launcher and the vault for iOS
(`aarch64-apple-ios`), Windows (`x86_64-pc-windows-msvc`) and Linux
(`aarch64-unknown-linux-gnu`) passes from the Mac; the last two with blake3's
`pure` feature for the duration, as `scripts/check-windows.sh` does. On the
Mac: `cargo fmt --check`; `rustup run 1.100.0 cargo clippy --workspace
--all-targets --all-features -- -D warnings`; `cargo check --workspace
--all-features`.

## Not done

- **Nobody but the tests and a script has used it.** No password of Jay's
  has been in it.
- **A phone.** The iPhone launcher has no Vault button: the iOS host has no
  on-screen keyboard for a loadngo window and its clipboard functions do
  nothing. A launcher built for a phone still carries the vault's code and
  its two crates, unused.
- **Linux and Windows by hand.** The code is the same and their compile is
  CI's to check; the clipboard there (`arboard`) and the fields under a
  Linux desktop have not been tried.
- **Changing the master password**, and raising the Argon2id costs of an
  existing file.
- **Getting passwords in and out in bulk**: no import from a browser or
  another manager, no export but `open-vault.py --show`.
- **Search.** The list is by name and scrolls.
- **Marking a copied password as a secret** for clipboard managers (macOS's
  `org.nspasteboard.ConcealedType`), and a keychain or Touch ID in place of
  typing the master password.
- **No audit.** Nobody but its author has read the vault's code. Of the two
  borrowed crates, `chacha20poly1305` was reviewed by NCC Group (public
  report of February 2020, on the code as it was then, with no significant
  finding); a search found no audit of `argon2`.
- Mouse presses were not made on the real window (a script's clicks do not
  reach a loadngo window reliably): the buttons are known from the unit
  tests, the keys from the real window.
