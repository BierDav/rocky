# Commit messages

Every commit on `main` follows [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/).
Release versions and notes are computed from them ([releasing](releasing.md)).

```text
type(scope)!: description

Optional body.

BREAKING CHANGE: optional footer, equivalent to `!`.
```

| Type | Use | Version effect |
| --- | --- | --- |
| `feat` | New behavior | Minor from 1.0; patch before |
| `fix` | Bug fix | Patch |
| `perf`, `refactor`, `revert` | Performance, restructuring, reverting | Patch |
| `docs`, `test`, `build`, `ci`, `chore`, `style` | No shipped behavior change | Patch |
| any with `!` or `BREAKING CHANGE:` | Incompatible change | Major from 1.0; minor before |

The scope is optional, lowercase, and usually a crate or area: `fix(rocky-secure): ...`,
`docs(features_05): ...`. Keep the header to one line, followed by a blank line before a body.

## Checking

`scripts/commitlint` holds the rules. It checks one message file, or every described
commit of a Jujutsu revset:

```sh
scripts/commitlint --jj                  # trunk()..@, skipping an undescribed working copy
scripts/commitlint --jj 'main@origin..'  # before pushing
```

Jujutsu does not run Git hooks, so check before `jj git push`. A repository alias helps:

```sh
jj config set --repo aliases.lint '["util", "exec", "--", "scripts/commitlint", "--jj"]'
jj lint
```

`scripts/install-hooks` links `scripts/hooks/commit-msg` into `.git/hooks` for commits made with
the Git CLI. The [Commit messages workflow](../.github/workflows/commits.yml) checks every pushed
and pull-request commit, so nothing unchecked reaches `main`. Git's own `Merge`, `Revert "` and
`fixup!`/`squash!`/`amend!` headers are accepted.
