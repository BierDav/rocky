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
`docs(features_05): ...`.

## Rules

- The header is `type(scope)!: description`, at most 100 characters.
- The description starts in lower case, except for acronyms such as `SQLite`,
  and ends without a period.
- A blank line separates the header from the body.
- A breaking change footer is exactly `BREAKING CHANGE: description`
  (or `BREAKING-CHANGE:`), in upper case.
- Git's own `Merge` and `Revert "` headers are accepted. `fixup!`, `squash!`
  and `amend!` commits are accepted on topic branches but refused on `main`
  and in pull requests into it; squash them first.

## Checking

`scripts/commitlint` holds the rules; `scripts/test-commitlint` pins them. It
checks one message file, or every described commit of a Jujutsu revset:

```sh
scripts/commitlint --jj                              # trunk()..@, skipping an undescribed working copy
scripts/commitlint --protected --jj 'trunk()..'      # as CI checks main
```

After [`scripts/setup`](linting.md#setup), `jj push` checks the messages it
would publish, and Git CLI commits run the `commit-msg` hook.
The [Commit messages workflow](../.github/workflows/commits.yml) checks every
pushed and pull-request commit and the pull-request title, which a squash merge
uses as the header. For a pull request it runs the base revision's
`scripts/commitlint`, so a change cannot relax the rules it is checked against.
