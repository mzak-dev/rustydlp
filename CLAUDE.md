# rustydlp

## Agent skills

### Issue tracker

Issues live as GitHub issues, managed via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical triage roles, used verbatim as label strings. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context — one `CONTEXT.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### Commit messages

Conventional Commits (Angular convention) — required, since `release-please` reads them to version and changelog releases. See `docs/agents/commit-conventions.md`.

### Releases

Three paths — automated (release-please), manual (`workflow_dispatch`), and a `-preview` prerelease per pull request — over one shared version script. See `docs/agents/releases.md`.
