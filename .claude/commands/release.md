---
model: haiku
---
Tag every CHANGELOG.md version that isn't tagged yet, and — only when the
version closes a value-block phase — publish a GitHub Release. `/merge`
already bumps `Cargo.toml`/`CHANGELOG.md` per PR; nothing currently tags or
releases, so this command closes that gap.

## Usage

```
/release              Tag every untagged version in CHANGELOG.md; open a
                       GitHub Release only for versions that close a phase
/release <version>    Tag/release just that one version (e.g. `/release 0.17.7`)
/release --dry-run    Show the plan without tagging, pushing, or releasing
```

## Background — how this repo actually versions

- `/merge` creates a `chore: release vX.Y.Z` commit per merged PR (feat →
  minor, fix → patch) with the `CHANGELOG.md` section and `Cargo.toml` bump,
  then pushes to `main`. It does **not** create a git tag or a GitHub
  Release — that gap is why this command exists.
- Not every version gets a GitHub Release. Check `gh release list`: only
  v0.4.0, v0.8.0, v0.12.3, v0.14.1, v0.17.0 have releases — each one is the
  version that finished a value-block phase (V1..V6), titled
  `V{N} — <short phase name>`. Every other tagged version (v0.17.1..v0.17.7,
  etc.) is a plain annotated tag with no release. Don't create a release for
  a version just because it's the newest tag.
- `tools/assurance.py`'s `VERSION_MAP` dict is the source of truth for which
  minor version closes which plan phase and which epic issue tracks it
  (`#5`, `#56`, `#161`, `#234`, ... `#421` for V7). Read it — don't guess
  from `plan.md` version numbers alone, since actual releases sometimes
  diverge from the original plan (see `CHANGELOG.md`'s versioning-policy
  note about the 0.4.0–0.6.0 renumbering).
- A version also needs the corresponding epic issue's phase checklist fully
  checked off (`gh issue view <epic>`) before it counts as "closes a phase."

## Process

### 1. Find versions needing a tag

```bash
git fetch --tags
grep -oE '^## \[[0-9]+\.[0-9]+\.[0-9]+\] - [0-9-]+' CHANGELOG.md   # released versions
git tag -l | sed 's/^v//' | sort -V                                # already tagged
```

Skip anything still under `## [Unreleased]` — not finalized yet. Diff the
two lists for what needs tagging (or just the one version passed as an
argument).

### 2. Tag each missing version at its own release commit

```bash
COMMIT=$(git log --all --format='%H %s' | grep -F "chore: release v$VERSION" | head -1 | cut -d' ' -f1)
git tag -a "v$VERSION" "$COMMIT" -m "chore: release v$VERSION"
```

Never tag a different commit — the `chore: release` commit is what pins
`Cargo.toml`/`CHANGELOG.md` to that version. If no such commit exists,
stop and report it (the version may have been hand-edited without going
through `/merge`).

### 3. Decide if the new tag also gets a GitHub Release

Cross-check against `VERSION_MAP` in `tools/assurance.py` and the linked
epic's checklist:

- **Closes a phase** (last checklist item in the epic ticks over at this
  version, or the epic issue itself closes at this tag): draft a release.
  - **Title:** `V{N} — <short phase name>`, matching the terse existing
    style (`gh release view v0.17.0` → "V6 — Concurrency"), not the full
    `plan.md` heading.
  - **Body:** the `CHANGELOG.md` sections for every version since the
    previous release tag, concatenated verbatim (Keep-a-Changelog prose
    already matches release-note tone — don't paraphrase or compress). Add
    a `Closes #a, #b` line for the epic's discharged sub-issues if not
    already present.
- **Does not close a phase:** tag only, no `gh release create`. Say so
  explicitly rather than silently skipping.

If it's ambiguous whether a version closes a phase (checklist partially
checked, epic still clearly open), ask the user — don't guess.

### 4. Push tags

```bash
git push origin "v$VERSION"     # or `git push --tags` for a batch
```

Confirm before pushing — this is the one shared-state, hard-to-reverse
step. Never force-move or re-tag an existing tag; if a tag already exists
for a version, skip it and report that it was already there.

### 5. Create the GitHub Release (only when step 3 said yes)

```bash
gh release create "v$VERSION" --title "V{N} — <name>" \
  --notes-file <(assembled CHANGELOG body) --latest
```

Only pass `--latest` when this is genuinely the newest tag.

## Output

```
Release Check
═══════════════════════════════════════════════════════════════
Untagged versions in CHANGELOG.md: 0.17.4, 0.17.5, 0.17.6, 0.17.7

Tagging:
  ✓ v0.17.4 → a1b2c3d (chore: release v0.17.4)
  ✓ v0.17.5 → d4e5f6a
  ✓ v0.17.6 → 5573f14
  ✓ v0.17.7 → 6d27c76

Phase check (VERSION_MAP): none of these close a value-block phase —
no GitHub Release created.

Pushed 4 tags to origin.
```

When a phase does close:

```
v0.18.0 closes V7 phase 2 (epic #421) — creating GitHub Release
  Title: V7 phase 2 — PRAGMAs & Introspection
  Notes: 6 CHANGELOG entries since v0.17.7 (v0.17.7..v0.18.0)

✓ Tagged v0.18.0
✓ Pushed
✓ Release published: https://github.com/iheitlager/sqlite-rs/releases/tag/v0.18.0
```

## Dry Run

`/release --dry-run` runs the full check (steps 1–3) but stops before
step 4 — no `git tag`, no `git push`, no `gh release create`. Always
run this first when unsure whether a version closes a phase.

```
/release --dry-run

Dry Run: Release Check
═══════════════════════════════════════════════════════════════
Untagged versions in CHANGELOG.md: 0.17.4, 0.17.5, 0.17.6, 0.17.7, 0.18.0

Would tag (plain, no release):
  v0.17.4 → a1b2c3d (chore: release v0.17.4)
  v0.17.5 → d4e5f6a
  v0.17.6 → 5573f14
  v0.17.7 → 6d27c76

Would tag + release (VERSION_MAP: closes V7 phase 2, epic #421):
  v0.18.0 → 9f0e1d2 (chore: release v0.18.0)
    Title: V7 phase 2 — PRAGMAs & Introspection
    Notes preview: 6 CHANGELOG entries, v0.17.7..v0.18.0
    Closes: #452, #453, #460 (epic #421 checklist items ticked by this version)

No tags created, nothing pushed, no release published.
Run without --dry-run to execute.
```

## Safety

- Never tags a commit other than its own `chore: release vX.Y.Z` commit.
- Never re-tags, force-moves, or deletes an existing tag.
- Never creates a GitHub Release for a version that doesn't clearly close a
  phase per `VERSION_MAP` + the epic checklist — ask rather than guess.
- Always confirms before `git push` and before `gh release create`.
- Skips (doesn't tag) any version still under `## [Unreleased]`.
