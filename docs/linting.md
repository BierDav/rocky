# Formatting and lint enforcement

Every rule is enforced at three layers with the same scripts, so a local pass
predicts the CI result:

| Layer | Formatting and lint | Commit messages |
| --- | --- | --- |
| Editing | `jj fix`, `scripts/lint --fix` | — |
| Before publishing | `jj push` (`scripts/jj-push`); Git `pre-commit` hook | `jj push`; Git `commit-msg` hook |
| CI, authoritative | `lint` job of [Baseline checks](../.github/workflows/check.yml) | [Commit messages](../.github/workflows/commits.yml) |

Local gates are a convenience: they can be bypassed, CI cannot. Make the
`lint`, `check` and commit-message jobs required status checks for `main` in
the forge's branch protection; the workflow files cannot configure it.

## Setup

```sh
scripts/setup
```

It is idempotent and only touches what it owns:

- `scripts/tools` downloads the tools pinned in `scripts/tools.lock` into
  `local/bin` and refuses any file whose SHA-256 differs from its pin.
- In a Jujutsu repository it sets repository-level config (outside the working
  copy): the `jj lint` and `jj push` aliases, and a `jj fix` tool for every
  lint module that declares a formatter.
- In a colocated Git repository it links `scripts/hooks/*` into `.git/hooks`
  and removes links to hooks that no longer exist.

Java must be on `PATH` for ktlint. To change a tool version, update its lines
in `scripts/tools.lock`, one per platform; the lock file documents the format.

## What is checked

`scripts/lint` needs no build and runs in seconds. Each language or file kind
is a module in `scripts/lint.d`; it runs every module, or the ones named
(`scripts/lint kotlin shell`), and reports every failing module, not only the
first:

| Module | Tool | Scope | Rules |
| --- | --- | --- | --- |
| `rust` | rustfmt | The workspace and `sdks/native` | `rustfmt.toml`, edition 2024 |
| `kotlin` | ktlint | `examples/**` and `sdks/kotlin/**` Kotlin | `.editorconfig` |
| `shell` | shellcheck | Scripts found by shebang, not vendored wrappers or benchmarks | defaults |
| `workflows` | actionlint | `.github/workflows`, including embedded scripts | defaults |
| `commits` | — | `scripts/test-commitlint` | [Commit messages](commits.md) |

`scripts/check` adds the build-dependent gates: Clippy with `clippy::all`
denied and warnings as errors (also declared in `Cargo.toml`, so editors agree),
then the tests. Benchmarks follow [their own rules](../benchmarks/engine-comparison/AGENTS.md).

## Jujutsu workflow

Jujutsu runs no hooks, so formatting is applied to revisions and checks run
at push time:

```sh
jj fix                    # format rustfmt/ktlint files in every mutable revision
jj lint                   # what `jj push` checks, without pushing
jj push --bookmark main   # check, then forward the arguments to `jj git push`
```

`jj push` checks the commits the push would publish: those between the
remotes and the selected bookmarks, revisions or changes, or the working copy
by default. It lints their messages and runs `scripts/lint` on the working
copy. CI checks every pushed tree.

## Adding a language or SDK

A new SDK adds files rather than editing the gates; CI changes only when a
tool needs a runtime its image lacks:

1. Pin its formatter or linter in `scripts/tools.lock`, one line per platform,
   unless it comes with the language toolchain as rustfmt does.
2. Add `scripts/lint.d/<language>`, a POSIX shell file sourced from the
   repository root with `local/bin` on `PATH`. It defines:
   - `tools`: the pinned tools it needs, checked before it runs;
   - `check()`: fails on any violation without changing files;
   - optionally `fix()`: applies the formatter; `scripts/lint --fix` runs it;
   - optionally `jj_fix_command` and `jj_fix_patterns`: a stdin-to-stdout
     formatter as TOML arrays, which `scripts/setup` registers for `jj fix`.
     `$root` and `$path` are the workspace root and the file's path.
   [`kotlin`](../scripts/lint.d/kotlin) shows every part.
3. Put the language's style settings in a `.editorconfig` section or its own
   configuration file at the repository root.
4. If the tool needs a runtime the CI image lacks, as ktlint needs Java, add its
   setup step to the `lint` job in [check.yml](../.github/workflows/check.yml).
5. Add build-dependent checks, like Clippy, to the SDK's own check script, and
   use the SDK's directory or package name as the commit scope.

New shell scripts are found by their shebang; no list needs updating.
