# Releases

Three paths produce a binary, and they share one piece of code —
`.github/scripts/version.py`, which does the version arithmetic and writes the
result into the files that carry a version. Anything about *how* a version is
computed or stamped belongs there, not in a workflow.

| path | trigger | workflow | tag | published as |
|---|---|---|---|---|
| automated | merging the release PR | `.github/workflows/rust.yml` | `vX.Y.Z` | release |
| manual | Actions → "Manual release" | `.github/workflows/release.yml` | `vX.Y.Z` | release (or prerelease/draft) |
| preview | any pull request | `.github/workflows/pr-preview.yml` | `vX.Y.Z-preview.<pr>` | prerelease |

## The three files that carry a version

`Cargo.toml` is the real one. `Cargo.lock` carries the crate's own version as
well, and a stale one fails every later `--locked` build — which is now every
build, CI included. `.release-please-manifest.json` is what release-please
reads as "the last release": a path that publishes and leaves it behind makes
the *next* automated release bump from a baseline that was never released, and
try to cut a version that already exists.

So the rule is: a path that publishes a release moves all three
(`version.py write V --manifest`); a path that doesn't, moves the first two
only (`version.py write V`). Preview builds are the second kind — they stamp
the binary they are about to build and touch nothing release-please reads.

## Automated (the normal path)

release-please reads Conventional Commits on `main` (see
[commit-conventions.md](commit-conventions.md)) and keeps a standing
`chore(main): release X.Y.Z` PR carrying the version bump and changelog that
history warrants. Merging that PR is the release: the merge push is what makes
`release_created` true, which cuts the tag and the GitHub release and lets the
same run build the Windows binary and attach it.

Tags are plain `vX.Y.Z` — `include-component-in-tag: false` in
`release-please-config.json`. Left at its default, release-please prefixes the
crate name (`rustydlp-v0.6.0`), which would put automated releases in a
different tag namespace from the two paths below and from every tag this repo
already has.

**The release PR needs a `RELEASE_PLEASE_TOKEN` secret to get its own CI.** The
default `GITHUB_TOKEN` deliberately cannot trigger workflow runs from its own
commits, so a release PR it opens sits with no checks on it — which is what has
happened to every release PR here so far. Set the secret to a PAT or GitHub App
token with `contents` and `pull-requests` write and the release PR runs `test`
like any other. Without it the workflow still works; the PR just has to be
approved by hand in the Actions tab before its checks run.

## Manual (the escape hatch)

Run "Manual release" from the Actions tab with an exact `version` (or a
`patch`/`minor`/`major` bump), optionally as a `prerelease`, a `draft`, or a
`dry_run` that only prints the plan. Reach for it for a hotfix, an `-rc` build,
or to redo a release whose build or upload failed. It refuses to reuse an
existing tag, and it moves the manifest, so the automated path keeps bumping
from the right baseline afterwards.

## Preview (per pull request)

Every pull request is built into a real Windows `.exe` and published as a
prerelease, so a change can be *run* before it lands rather than only read.

- **Version.** The base version's patch bumped, plus `-preview.<pr number>`:
  `0.5.1-preview.18` for PR #18 against a repo at 0.5.0. The patch bump is what
  makes it sort correctly — `0.5.0-preview.18` would order *before* 0.5.0, and
  the branch holds work that comes after it, not a run-up to it. The PR number
  keeps two open pull requests off the same tag.
- **Rolling, not accumulating.** One release per pull request, replaced on every
  push and deleted when the pull request closes. The tag is deleted with it,
  which is the part that matters: a tag left pointing into a deleted branch
  keeps that commit reachable forever. Cleanup matches on the `-preview.<pr>`
  suffix rather than an exact tag, so it still finds the previous preview when a
  release landed on `main` and moved the base version mid-review.
- **Built from the head commit**, not the merge ref — the preview is a build of
  that branch, and the merge ref is ephemeral and could not be tagged anyway.
- **Forks get an artifact, not a release.** A fork's `GITHUB_TOKEN` is read-only
  no matter what the workflow asks for, so publishing is skipped there; the
  build still runs and uploads the `.exe` as a workflow artifact.
- **release-please's own PR is skipped.** It carries nothing but a version bump
  and a changelog, and its `Cargo.toml` already holds the version being
  proposed, which would make its preview read a release ahead of anything real.

A preview release is never something to link to as though it were a release: it
is marked prerelease, its notes say so, and it will be gone when the pull
request closes.

Marking them prerelease is load-bearing twice over, not just cosmetic.
`/releases/latest` and the "Latest" badge both skip prereleases, so an open pull
request cannot become what the repo appears to ship; and release-please's search
for the last release skips them too (it only considers prereleases when the
config asks it to, which this one does not), so a preview tag cannot be mistaken
for the baseline the next automated release bumps from.
