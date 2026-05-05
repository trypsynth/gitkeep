# gitkeep

A high-performance CLI tool to manage and maintain local archives of GitHub users and organizations.

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

- Setup: `gitkeep init`
- Authenticate: `gitkeep login`
- Track a user: `gitkeep add trypsynth`
- Sync everything: `gitkeep run`
- Show tracked list: `gitkeep list`
- Background sync: `gitkeep watch`

## Commands

Usage: `gitkeep <COMMAND>`

- `init` Create or reset the config file interactively
- `login` Authenticate with a GitHub personal access token
- `add <USERS>` Add users or orgs to the archive list
- `remove <USERS>` Stop tracking one or more users or orgs
- `list` (alias: `ls`) Show all tracked users and orgs
- `run` (alias: `sync`) Sync all tracked users immediately
- `watch` Daemon mode: sync on a schedule

## License

`gitkeep` is licensed under the [MIT License](LICENSE).
