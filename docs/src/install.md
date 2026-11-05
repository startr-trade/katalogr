# Install

> **No public release exists yet.** The install script below does not work until the first public
> release is published. Until then, build from source.

## From a release

Once a release exists, install the latest with:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://github.com/startr-trade/katalogr/releases/latest/download/install.sh | sh
```

## From source

You need [rustup](https://rustup.rs). The repository's `rust-toolchain.toml` selects the
toolchain.

```sh
git clone https://github.com/startr-trade/katalogr
cd katalogr
cargo build --release --locked -p katalogr
```

The binary is `target/release/katalogr`. Building with `--no-default-features` leaves out the
optional MCP server.

## Other agent hosts

Claude Code uses the katalogr skill and hooks. Hosts that cannot, such as Codex, Cursor and
Windsurf, run `katalogr mcp --root /path/to/repo` as an MCP server over stdio. The repository
README has the configuration for each.
