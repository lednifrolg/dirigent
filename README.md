# dirigent

Terminal client for the IKEA DIRIGERA smart home hub over its local API: an interactive TUI and
one-shot commands for the shell.

Unofficial project, not affiliated with IKEA. It talks to the hub's undocumented local API and
switches real devices in a real home.

## Status

Early development: the workspace and CI are set up, the `dirigent` CLI does not do anything yet.
The local API has been verified live for pairing, reading the home state and switching a light,
on hub firmware 2.975.3.

## Pairing

`dirigent pair` finds the hub on the network and asks you to press the action button on the
bottom of the hub. The token the hub issues is kept locally, readable only by you, together with
the fingerprint of the hub's certificate. Nothing leaves your home network.

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

Test data in `crates/dirigera/tests/data` is synthetic. Never commit captured hub responses,
tokens or real device identifiers. Code, docs and commit messages are in English.

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
