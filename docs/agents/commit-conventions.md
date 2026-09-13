# Commit messages: Conventional Commits (Angular)

`release-please` (`.github/workflows/rust.yml`) reads Conventional Commits on
`main` to decide the next version and to write the changelog. A commit that
doesn't follow the format is invisible to it — at best it's omitted from the
changelog, at worst it silently fails to trigger the version bump it should
have.

## Format

```
<type>(<scope>): <subject>

<body>

<footer>
```

- **type** — one of: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`,
  `test`, `build`, `ci`, `chore`, `revert`.
- **scope** — optional, the area touched (e.g. `ui`, `ytdlp`, `ci`). Omit it
  rather than guess one that doesn't fit.
- **subject** — imperative, present tense, no trailing period: "add" not
  "added"/"adds".
- **body** — optional, explains *why* over *what*, same as any commit here.
- **footer** — `BREAKING CHANGE: <description>` when the change breaks a
  public API or on-disk format; otherwise omit it.

Breaking changes may instead mark the type/scope with `!` (`feat!:`,
`fix(ui)!:`) — pick one form, don't do both.

## What bumps what

- `fix:` → patch release.
- `feat:` → minor release.
- `BREAKING CHANGE:` footer or `!` → major release.
- Everything else (`docs`, `style`, `refactor`, `test`, `build`, `ci`,
  `chore`) → no release, changelog entry only (some types are grouped or
  hidden entirely under release-please's default Angular config).

## Applies to

Every commit merged to `main` — direct pushes and squash-merged PRs alike,
since release-please reads the commit log, not PR titles. A branch's
intermediate commits matter less if the PR is squash-merged, but write them
as Conventional Commits anyway so `git log` stays readable and so nothing is
lost if the PR is merged with "Create a merge commit" instead.
