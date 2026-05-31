# gitkeep

A CLI tool for maintaining local archives of GitHub users and organizations. Point it at one or more accounts and it clones every repo, then keeps them up to date on subsequent runs.

## Download

Pre-built binaries are available on the [releases page](https://github.com/trypsynth/gitkeep/releases/latest).

| Platform | Architecture | Download |
|----------|-------------|---------|
| Linux | x86_64 | [gitkeep-x86_64-unknown-linux-musl](https://github.com/trypsynth/gitkeep/releases/latest/download/gitkeep-x86_64-unknown-linux-musl) |
| Linux | ARM64 | [gitkeep-aarch64-unknown-linux-musl](https://github.com/trypsynth/gitkeep/releases/latest/download/gitkeep-aarch64-unknown-linux-musl) |
| macOS | x86_64 | [gitkeep-x86_64-apple-darwin](https://github.com/trypsynth/gitkeep/releases/latest/download/gitkeep-x86_64-apple-darwin) |
| macOS | ARM64 (Apple Silicon) | [gitkeep-aarch64-apple-darwin](https://github.com/trypsynth/gitkeep/releases/latest/download/gitkeep-aarch64-apple-darwin) |
| Windows | x86_64 | [gitkeep-x86_64-pc-windows-gnu.exe](https://github.com/trypsynth/gitkeep/releases/latest/download/gitkeep-x86_64-pc-windows-gnu.exe) |
| Windows | ARM64 | [gitkeep-aarch64-pc-windows-gnullvm.exe](https://github.com/trypsynth/gitkeep/releases/latest/download/gitkeep-aarch64-pc-windows-gnullvm.exe) |

## Install

### With cargo

```bash
cargo install gitkeep
```

### From source

```bash
git clone https://github.com/trypsynth/gitkeep
cd gitkeep
cargo install --path .
```

## Quick start

```bash
gitkeep init          # set archive directory and clone URL preference
gitkeep login         # authenticate with a GitHub personal access token
gitkeep add trypsynth # add a user (clones all their repos immediately)
gitkeep sync          # sync everything
gitkeep list          # show what's tracked
```

## Commands

### `init`
Configure the archive directory and whether to use SSH or HTTPS clone URLs. Re-run at any time to update settings; your token and tracked users are preserved.

### `login`
Authenticate with a GitHub personal access token. Opens the token creation page in your browser, validates the token, and saves it to config. Also adds your own account to the tracked list automatically.

### `add <USERNAME>...`
Add one or more GitHub users or orgs to the archive list and clone their repos immediately.

| Flag | Description |
|------|-------------|
| `--forks` | Include forked repositories for these accounts |
| `--frozen` | Track the account but never update it during bulk syncs |
| `--no-sync` | Add to the tracked list without cloning right now |

### `sync [USERNAME]...`  _(alias: `run`)_
Sync all tracked accounts. Passing usernames adds them to the tracked list and syncs them immediately (same as `add` + `sync`).

| Flag | Description |
|------|-------------|
| `--forks` | Include forks for this run only (does not save to config) |
| `-p, --pull-only` | Only pull existing repos; skip checking for new ones |
| `-n, --new-only` | Only clone new repos; skip pulling existing ones |
| `-q, --quiet` | Suppress all output except errors and the final summary |
| `-v, --verbose` | Show raw git output and per-repo detail |

### `skip <user/repo>...`
Exclude a specific repo from future syncs. Accepts `user/repo` format.

### `unskip <user/repo>...`
Re-enable a previously skipped repo.

### `list`  _(alias: `ls`)_
Show all tracked users and orgs, including any per-account flags and the list of skipped repos.

### `remove <USERNAME>...`  _(alias: `rm`)_
Stop tracking one or more users or orgs. Prompts to delete the local archive directory; pass `--delete` to skip the prompt.

## License

`gitkeep` is licensed under the [MIT License](LICENSE).
