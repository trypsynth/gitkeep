# Changelog

## Unreleased

- Added a per-host SSH setting. The first time you add something from a new host, gitkeep asks whether to clone from it over SSH; set `use_ssh` under that host in the config to change it later
- Fixed a repo with no clone URL of the chosen kind (SSH or HTTPS) being silently skipped. It's now reported as a failure
- `gitkeep size` now shows just the total by default. Pass `--breakdown` to also list each account's size, including each account on other forges instead of one line per host
- Repos are now cloned and pulled, and account listings fetched, several at a time (one per CPU thread, between 4 and 16), which makes big syncs several times faster. Change it with `gitkeep sync -j <N>` or `jobs = <N>` in the config

## 0.3.1

- A relative archive directory in the config is now relative to your home directory instead of wherever gitkeep is run from
- Fixed `gitkeep remove user/repo` not offering to delete the local copy of an individually tracked repo
- Fixed the unauthenticated mode warning being printed more than once per command
- `gitkeep add` now prints one combined summary when adding accounts and individual repos together
- `gitkeep remove <owner>` no longer asks before untracking the repos you track individually under it, only before deleting them
- `gitkeep remove` now also removes the owner, group, and host directories that deleting the last repo under them leaves empty

## 0.3.0

- A failed clone or pull now shows git's own error message
- `add` and `remove` now accept `user/a,b,c` to name several repos from one owner
- Added Forgejo support, which also covers Gitea and Codeberg. Repos are added by URL, the same way as GitLab
- Added GitLab support: `gitkeep add <url>` tracks a group (including subgroups), users, or single projects on any GitLab instance. GitLab archives live under `<host>/<namespace>/<project>`; `remove`, `sync`, and `list` accept the host-qualified `host/namespace/project` form or the full URL
- Added per-host tokens in the config for accessing private GitLab and Forgejo repos
- Fixed an archive directory starting with `~` being created as a folder literally named `~` in the current directory instead of under your home directory
- `gitkeep init` now saves a relative archive directory as an absolute path, so the archive no longer depends on where gitkeep is run from
- `gitkeep sync <targets>` now syncs only the named accounts and repos, including frozen ones
- Removed `gitkeep skip`, `unskip`, and `prune`. Run `gitkeep remove user/repo` to leave a repo under a tracked account out of future syncs (with `--delete` to also delete its local copy), and `gitkeep add user/repo` to bring it back. Existing skipped repos carry over automatically

## 0.2.1

- Added `gitkeep size` to show archive disk usage
- `gitkeep login` now asks before tracking your own account
- `gitkeep remove` now offers to delete leftover archive directories for users you no longer track
- Cleaned up directories for users you only tracked individual repos from after removing their whole archive
- Fixed leftover archive directories being shown with the wrong casing on case-insensitive filesystems
- Removed the "Resolved x to y" message when adding users
- Updated dependencies

## 0.2.0

- A repo whose local checkout no longer matches what's on GitHub (deleted and recreated under the same name) is now automatically re-cloned instead of left stale
- Added a `--delete` flag to `gitkeep skip`
- Added a `-y, --yes` flag to `gitkeep remove` to skip confirmation prompts
- Added a configurable default for `--no-sync`
- Added `gitkeep prune` to delete local copies of all skipped repos
- Added per-account and per-sync submodule support, plus a configurable global default
- Added support for pinning individual repos with `gitkeep add user/repo`
- Fixed the delete prompt being skipped when a username's case differs from its directory name
- Improved removing a user when only individually pinned repos are tracked for them
- Made `gitkeep list` filter out deleted repos
- Tracked accounts and repos now store their stable GitHub IDs, so renamed users and repos are resolved correctly

## 0.1.0

Initial release
