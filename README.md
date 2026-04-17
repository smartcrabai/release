# release

A small Rust CLI that bumps a project's version, commits, tags, pushes, and
optionally publishes — across a handful of package managers.

## Usage

```sh
release [patch|minor|major] [--no-publish] [--dry-run] [--backend <name>]
```

The positional argument defaults to `patch`.

### Flags

| Flag            | Description                                                                |
| --------------- | -------------------------------------------------------------------------- |
| `--no-publish`  | Skip the publish step (existing `cargo-release` compatibility).            |
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
| dotnet  | `*.csproj` / `*.fsproj` / `Directory.Build.props`     | `<Version>...</Version>`                        | `dotnet restore`             | `dotnet pack -c Release` (API key is yours)   |
| go      | `go.mod`                                              | — (tag only; previous tag is read from git)     | —                            | — (no-op; tags are the release)               |

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
