# dirigent

Terminal client for the IKEA DIRIGERA smart home hub over its local API: an interactive TUI and
one-shot commands for the shell.

Unofficial project, not affiliated with IKEA. It talks to the hub's undocumented local API and
switches real devices in a real home.

## Status

Early development: the one-shot commands work, and the TUI shows the home live; changing devices
from the TUI comes next. Verified live on hub firmware 2.975.3.

## Usage

`dirigent` without a command opens the interactive TUI: rooms with their devices, scenes and
remotes on the left, the selected one in detail, and every change in the home as it happens, also
from remotes and other apps. It pairs with the hub on its first start. Changing devices from the
TUI comes next; for now that is what the commands are for:

```sh
dirigent pair                 # finds the hub; press its action button when asked
dirigent list                 # rooms with their devices, scenes and remotes (--json for scripts)
dirigent status "led strip"   # one device, room or scene

dirigent on|off|toggle kitchen "desk lamp"    # devices or whole rooms
dirigent dim led 40           # or relative: +10, -10
dirigent white led warm       # or kelvin: 2700
dirigent color led orange     # or a hue with saturation: 30,0.8, or #ff8800
dirigent scene evening        # --undo takes the latest run back
dirigent identify led         # blinks for 5 s
dirigent watch                # every change as it happens, from remotes and apps too
```

Names match regardless of case and diacritics, and part of a name is enough when it is
unambiguous; `--room` or `--device` settles a name that is both. A command waits until the
device reports the change, at most 5 s; `--no-wait` returns once the hub took it.

## Pairing

`dirigent pair` finds the hub on the network and asks you to press the action button on the
bottom of the hub. The token the hub issues is kept in `~/.local/state/dirigent/hub.json`,
readable only by you, together with the fingerprint of the hub's certificate: later connections
accept only that certificate. When the hub's address changes, dirigent finds it again by its
identity. Nothing leaves your home network. `DIRIGENT_HOME` puts the file in another directory.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | the hub is unreachable or failed, or a local file problem |
| 2 | wrong usage: an unknown or ambiguous name, or a change the device cannot take |
| 4 | not paired, the hub no longer accepts dirigent, or its certificate changed |
| 5 | sent, but the device did not report the change in time |

## Using the API responsibly

- Pair once. Every pairing adds another user to the hub; `dirigent unpair` removes the one
  dirigent created.
- Every command switches a real device. Tests never reach a real hub, and live checks change one
  thing at a time and put it back.
- Never log or commit the token, serial numbers, MAC addresses or captured hub responses.

## Development

| Crate | Purpose |
|---|---|
| `crates/dirigera` | DIRIGERA local API client, no UI dependencies |
| `crates/dirigent` | the `dirigent` binary: CLI and TUI |

Test data in `crates/dirigera/tests/data` is synthetic, and tests run against mock hubs: the
CLI tests run the binary against a fake hub with `DIRIGENT_HUB_URL` pointing at it. Never
commit captured hub responses, tokens or real device identifiers. Code, docs and commit messages
are in English.

`cargo run -p dirigera --example probe` runs single requests against a real hub, for live checks.

The checks CI runs:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --locked
cargo deny check
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
