# dirigent

`dirigent` is a terminal client for the IKEA DIRIGERA smart home hub over its local API: an
interactive TUI and one-shot commands for the shell, on Linux, macOS and Windows. Verified live
on hub firmware 2.975.3.

Unofficial project, not affiliated with IKEA. It talks to the hub's undocumented local API and
switches real devices in a real home.

![The shell with dirigent list and dirigent dim, then the TUI switching and dimming a light, picking a color for an LED strip, running a scene and taking it back, and its help](assets/demo.gif)

## Install

From source, with a recent Rust:

```sh
cargo install --git https://github.com/lednifrolg/dirigent --locked dirigent
```

or from a clone, `cargo install --path crates/dirigent --locked`. On Windows, the build needs the
Visual Studio C++ build tools besides Rust; rustup offers to install them. Shell completions and
man pages come from the binary:

```sh
dirigent completions bash > ~/.local/share/bash-completion/completions/dirigent
dirigent completions zsh > ~/.zfunc/_dirigent      # a directory on your $fpath
dirigent completions fish > ~/.config/fish/completions/dirigent.fish
dirigent man ~/.local/share/man/man1
```

In PowerShell, add `dirigent completions powershell | Out-String | Invoke-Expression` to your
`$PROFILE`.

## Usage

```sh
dirigent pair                 # finds the hub; press its action button when asked
dirigent                      # the interactive TUI
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
`dirigent <command> --help` has the details.

The TUI shows rooms with their devices, scenes and remotes on the left, the selected one in
detail, and every change in the home as it happens, also from remotes, scenes and other apps.
It pairs with the hub on its first start.

| Key | Does |
|---|---|
| `↑` `↓` | select a room, device, scene or remote |
| `␣` `⏎` | switch on or off, a whole room at once; run a scene |
| `←` `→` | brightness ±10 %, with Shift ±1 % |
| `[` `]` | warmer or cooler white |
| `c` | a color picker: presets, any hue and saturation, or white |
| `i` | make the device blink for 5 s |
| `u` | take back the latest change, or a scene while the hub allows |
| `R` | read everything from the hub again |
| `?` | every key |

A change shows at once and goes out right away; the activity pane says when the device reports
it. The mouse selects rows, switches a device by its dot, and turns a light's bars with the
wheel. In a narrow window, `Tab` switches between the devices and the activity.

## Pairing

`dirigent pair` finds the hub on the network and asks you to press the action button on the
bottom of the hub. The token the hub issues is kept in `~/.local/state/dirigent/hub.json`
(`%LOCALAPPDATA%\dirigent\state\hub.json` on Windows), readable only by you, together with the
fingerprint of the hub's certificate: later connections accept only that certificate. When the
hub's address changes, dirigent finds it again by its identity. Nothing leaves your home network.
`dirigent unpair` removes dirigent from the hub's users again.

## Configuration

`~/.config/dirigent/config.toml` (`%APPDATA%\dirigent\config.toml` on Windows) is optional:

```toml
animations = false   # no flashing rows, blinking dots or bars in motion in the TUI
```

`DIRIGENT_HOME` puts config and state in one directory. On Windows, the access rules of your user
profile keep these files private; a `DIRIGENT_HOME` outside it gets the rules of its directory.

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

`crates/dirigera` is the DIRIGERA local API client, without UI dependencies; `crates/dirigent`
the CLI and TUI. Test data in `crates/dirigera/tests/data` is synthetic, and tests run against
mock hubs: the CLI tests run the binary against a fake hub that `DIRIGENT_HUB_URL` points at.
Never commit captured hub responses, tokens or real device identifiers. Code, docs and commit
messages are in English.

`cargo run -p dirigera --example probe` runs single requests against a real hub, for live
checks. The same fake hub with a small home serves demos and screenshots, and
[`assets/demo.tape`](assets/demo.tape) records the GIF above with
[vhs](https://github.com/charmbracelet/vhs):

```sh
export DIRIGENT_HOME=$(mktemp -d) DIRIGENT_HUB_URL=http://127.0.0.1:18892
cargo run -p dirigent --example demo_hub -- 18892 "$DIRIGENT_HOME" &
cargo run -p dirigent

cargo build -p dirigent --bin dirigent --examples && vhs assets/demo.tape
```

CI runs:

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
