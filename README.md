# gitkeep

A CLI tool for maintaining local archives of GitHub and GitLab users and organizations. Point it at one or more accounts and it clones every repo, then keeps them up to date on subsequent runs.

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
Configure the archive directory, whether to use SSH or HTTPS clone URLs, whether to clone submodules by default, and whether repos are cloned immediately when added. Re-run at any time to update settings; your token and tracked users are preserved.

### `login`
Authenticate with a GitHub personal access token. Opens the token creation page in your browser, validates the token, and saves it to config. Also offers to add your own account to the tracked list (defaults to yes).

### `add <TARGET>...`
Add one or more GitHub users, orgs, or individual repos to the archive list and clone them immediately. A target is either a plain username/org (tracks the whole account) or `user/repo` (pins just that one repo). Adding `user/repo` for a repo you removed from a tracked account starts syncing it again. Several repos from one owner can be listed as `user/a,b,c`.

Repos on other forges are given as URLs (or as `host/path`): `gitkeep add https://gitlab.example.com/some-group` tracks a group (including subgroups), while `gitkeep add https://gitlab.example.com/some-group/project` adds just that one project. GitLab is currently supported. The first time you add something from a host, gitkeep detects which forge it runs and records it in the config. These archives are stored under `<host>/<namespace>/<project>`, and everywhere such a repo is referenced (`remove`, `sync`, `list`) uses the same `host/namespace/project` format, or the full URL. Public repos work without authentication; for private ones, add a personal access token for that host to the config:

```toml
[hosts."gitlab.example.com"]
kind = "gitlab"
token = "glpat-..."
```

| Flag | Description |
|------|-------------|
| `--forks` | Include forked repositories for these accounts |
| `--frozen` | Track the account but never update it during bulk syncs |
| `--submodules` | Clone submodules for these accounts/repos, overriding the global default |
| `--no-submodules` | Never clone submodules for these accounts/repos, overriding the global default |
| `--no-sync` | Add to the tracked list without cloning right now, overriding the config default |
| `--sync` | Clone immediately after adding, overriding a `no_sync = true` config default |

### `sync [TARGET]...`  _(alias: `run`)_
Sync every tracked account that isn't frozen, plus individually tracked repos. Pass targets (in the same forms as `add`) to sync only those instead, including frozen accounts; naming a repo inside a tracked account syncs that account. Targets that aren't tracked yet are added first, just like `add`.

| Flag | Description |
|------|-------------|
| `--forks` | Include forks for this run only (does not save to config) |
| `--submodules` | Clone submodules for this run only (does not save to config) |
| `-p, --pull-only` | Only pull existing repos; skip checking for new ones |
| `-n, --new-only` | Only clone new repos; skip pulling existing ones |
| `-q, --quiet` | Suppress all output except errors and the final summary |
| `-v, --verbose` | Show raw git output and per-repo detail |

A sync that fails to pull an existing repo because it no longer matches what's on GitHub (e.g. the `owner/name` was deleted and recreated as a different repo) automatically deletes the stale local clone and re-clones it.

### `list`  _(alias: `ls`)_
Show all tracked users and orgs, including any per-account flags, individually tracked repos, and repos removed from tracked accounts.

### `size`  _(alias: `du`)_
Show the on-disk size of the archive, broken down per account.

| Flag | Description |
|------|-------------|
| `-s, --format <FORMAT>` | Unit format: `decimal` (kB/MB/GB, base 1000), `binary` (KiB/MiB/GiB, base 1024, default), or `raw` (exact byte count) |

### `remove <TARGET>...`  _(alias: `rm`)_
Stop tracking one or more users, orgs, or repos. Accepts either a plain username/org or `user/repo`. A `user/repo` target can be an individually pinned repo or a single repo under a fully tracked account; the latter is left out of future syncs while the rest of the account keeps syncing (run `gitkeep add user/repo` to undo). Like `add`, it accepts `user/a,b,c` to name several repos from one owner. GitLab projects work the same way as `host/namespace/project` (or the URL), including removing one project from a tracked group. Prompts to delete the local archive directory; pass `--delete` to skip the prompt. If a target isn't tracked as a full user but has individually pinned repos under it, prompts to remove those too.

| Flag | Description |
|------|-------------|
| `-d, --delete` | Also delete the local archive directory for these targets |
| `-y, --yes` | Skip all confirmation prompts, assuming "yes" |

## Development

Formatting relies on unstable rustfmt options (see `rustfmt.toml`), which stable rustfmt silently ignores. Format with the nightly toolchain:

```bash
rustup toolchain install nightly --component rustfmt
cargo +nightly fmt
```

CI enforces this the same way, via `cargo fmt -- --check` on the nightly toolchain.

## License

`gitkeep` is licensed under the [MIT License](LICENSE).
