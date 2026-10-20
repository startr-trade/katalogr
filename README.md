# katalogr

Deterministic, technology- and protocol-neutral traceability graph for polyglot systems, built for humans and AI coding agents.

## Install

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://github.com/startr-trade/katalogr/releases/latest/download/install.sh | sh
```

## Other agent hosts (MCP)

Claude Code uses the katalogr skill and hooks instead (ADR-0004), which cost no tool-schema tokens; this section is for hosts that cannot, such as Codex, Cursor and Windsurf (ADR-0023). `katalogr mcp` serves the core queries (`find`, `refs`, `callers`, `explain`, `show`, `path`, `map`, `search`, `dangling`, `why`, `index`, `status`) as MCP tools over stdio, with the same `schema/v1` answers as the CLI's `--json`.

Hosts start servers from varying working directories, so pass `--root` with the repository the project is found from. Release binaries include `katalogr mcp`; a source build with `--no-default-features` leaves it out.

### Codex

In `~/.codex/config.toml`:

```toml
[mcp_servers.katalogr]
command = "katalogr"
args = ["mcp", "--root", "/path/to/repo"]
```

### Cursor

In `.cursor/mcp.json` or `~/.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "katalogr": {
      "command": "katalogr",
      "args": ["mcp", "--root", "/path/to/repo"]
    }
  }
}
```

### Windsurf

In `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "katalogr": {
      "command": "katalogr",
      "args": ["mcp", "--root", "/path/to/repo"]
    }
  }
}
```

## License

Licensed under either of Apache License 2.0 or MIT license at your option.
