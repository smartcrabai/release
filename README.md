# release

A small Rust CLI that bumps a project's version, commits, tags, pushes, and
optionally publishes — across a handful of package managers.

## Installation

### Homebrew

```sh
brew install smartcrabai/tap/release
```

### Shell installer (macOS / Linux)

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/smartcrabai/release/releases/latest/download/release-installer.sh | sh
```

### Cargo

```sh
cargo install --git https://github.com/smartcrabai/release
```

### Prebuilt binaries

Download the archive for your platform from the [GitHub Releases](https://github.com/smartcrabai/release/releases/latest) page. Supported targets:

- `aarch64-apple-darwin`
- `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`

## Usage

```sh
release [patch|minor|major] [-P|--no-publish] [-p|--only-publish] [--dry-run] [--backend <name>]
```

The positional argument defaults to `patch`.

### Flags

| Flag            | Description                                                                |
| --------------- | -------------------------------------------------------------------------- |
| `-P`, `--no-publish` | Skip the publish step (existing `cargo-release` compatibility).       |
| `-p`, `--only-publish` | Only run the publish step; skip bump/commit/tag/push (mutually exclusive with `--no-publish`). |
| `--dry-run`     | Print the steps that would run without writing files or running commands. |
| `--backend <n>` | Override auto-detection. One of `cargo`, `pnpm`, `bun`, `go`, `dotnet`, `julia`, `uv`. |

## Supported package managers

| Backend | Detection (first match wins)                          | Version location                                | Lockfile update              | Publish                                       |
| ------- | ----------------------------------------------------- | ----------------------------------------------- | ---------------------------- | --------------------------------------------- |
| cargo   | `Cargo.toml`                                          | `[package].version` / `[workspace.package].version` | `cargo generate-lockfile` | `cargo publish` (workspace: `-p <name>`)      |
| uv      | `pyproject.toml` + `uv.lock`                          | `[project].version`                             | `uv lock`                    | `uv publish`                                  |
| pnpm    | `package.json` + `pnpm-lock.yaml` (or `package.json` alone as fallback) | `"version"` in `package.json`      | `pnpm install --lockfile-only` | `pnpm publish --no-git-checks`              |
| bun     | `package.json` + `bun.lock` or `bun.lockb`            | `"version"` in `package.json`                   | `bun install`                | `bun publish`                                 |
| julia   | `Project.toml` containing `uuid` and `version`        | top-level `version`                             | — (Manifest is your problem) | — (no-op)                                     |
| dotnet  | `*.sln` / `*.csproj` / `*.fsproj` / `Directory.Build.props` | `<Version>...</Version>`                  | `dotnet restore`             | `dotnet pack -c Release` (API key is yours)   |
| go      | `go.mod`                                              | — (tag only; previous tag is read from git)     | —                            | — (no-op; tags are the release)               |

## Workspace / monorepo support

For projects that define multiple packages in a single repository the tool
updates every member's version in lockstep with the root version — reading
only the root version and writing the new version to every discovered member
manifest. Glob patterns use single-segment wildcards (`packages/*`,
`crates/*`, …); `**` and `!`-negated patterns are skipped with a warning.

| Backend | Workspace source                                             | Lockstep updates                                                    |
| ------- | ------------------------------------------------------------ | ------------------------------------------------------------------- |
| cargo   | `[workspace] members = [...]` (virtual or root-package)      | each member's `[package].version`; skipped when `[workspace.package].version` is used |
| uv      | `[tool.uv.workspace] members = [...]` in root `pyproject.toml` | each member's `[project].version`                                 |
| pnpm    | `pnpm-workspace.yaml` `packages:`                            | each member's `package.json` `"version"`                            |
| bun     | `"workspaces"` array or object in root `package.json`        | each member's `package.json` `"version"`                            |
| dotnet  | `Directory.Build.props` (centralized), `*.sln`, or recursive `*.csproj`/`*.fsproj` discovery | `<Version>` in each project file (no-op for projects lacking the element); central `Directory.Build.props` updates only that file |
| go      | — (no version files; the git tag is the release)             | — (not applicable)                                                  |
| julia   | — (no standard workspace layout)                             | — (not applicable)                                                  |

## Git workflow

For every backend the tool:

1. Requires a clean working tree and the `main` branch (warnings only under `--dry-run`).
2. Runs `git pull --ff-only origin main`.
3. Bumps the version (`patch` / `minor` / `major`).
4. Updates the lockfile where applicable.
5. Stages the manifest (and lockfile), commits as `chore: bump version to X.Y.Z`,
   tags `vX.Y.Z`, and pushes both `main` and the tag to `origin`.
6. Publishes unless `--no-publish` is passed.

Under `--dry-run` nothing is written, no external commands run, and no
git mutations happen — each intended action is logged as `would run: ...` or
`would write: ...`.
