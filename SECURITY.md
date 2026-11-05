# Security Policy

## katalogr's security model

katalogr indexes source code and documents on your own machine and answers questions about them. Its
attack surface is small by design:

- **No accounts, passwords, tokens or network auth.** There is nothing to log into.
- **Indexing reads; it does not execute.** katalogr parses the files of a repository. It does not
  build, run or fetch anything from the code it scans.
- **The index is local.** It is written to a cache folder on your machine. Scanned repositories'
  paths and code are kept out of katalogr's own repository; guard checks refuse to write absolute
  local paths into shareable output.
- **`katalogr mcp` talks over stdio** to the agent host that started it. It opens no network port.
- **Access control is yours.** Who can read an index, a repository or a CI artifact is decided by
  your file permissions and your git host, not by katalogr.

The trust boundary is your local machine and the repositories you point katalogr at. Treat an
index built from a private repository as private.

## Supported versions

katalogr is on a `0.x` line; only the **latest released version** is supported. Please reproduce
an issue against the newest release (or `main`) before reporting. There is no public release yet.

## Reporting a vulnerability

**Please do not open a public issue or pull request for security problems.**

Report it privately through GitHub's private vulnerability reporting: open
<https://github.com/startr-trade/katalogr/security/advisories/new> (Security tab, "Report a
vulnerability").

Please include: the affected version or commit, steps to reproduce, the impact, and any suggested
fix.

### What to expect

- Acknowledgement on a **best-effort** basis; this is a small project with no guaranteed response
  time.
- A fix or mitigation in a later release, with credit to you in the release notes unless you prefer
  to stay anonymous.

### In scope

- The `katalogr` CLI and its crates: for example, path traversal while scanning, a scan that reads
  or writes outside the repository or cache folder, or an index that discloses absolute local
  paths it is meant to refuse.
- The `katalogr mcp` server and the hooks the CLI installs.
- The release binaries and `install.sh` / `install.ps1`.

### Out of scope

- The security of your git remote, your agent host or your operating system.
- Vulnerabilities in third-party dependencies that do not affect katalogr's use of them. Report
  those upstream; we will bump the dependency.
- Malformed input that only makes katalogr report an error and exit cleanly.
