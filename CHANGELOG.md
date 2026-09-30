# Changelog

## Unreleased

- Added GitLab support: `gitkeep add <url>` tracks a group (including subgroups), users, or single projects on any GitLab instance. GitLab archives live under `<host>/<namespace>/<project>`; `remove`, `sync`, and `list` accept the host-qualified `host/namespace/project` form or the full URL. Optional per-host tokens can be set in the config.

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
