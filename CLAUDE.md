# Project Rules

These rules apply to all future work in this repository.

## Git & Commits
1. **Never push.** Do not run `git push` under any circumstances.
2. **Commit after completing work**, but do **not** add co-author trailers (no `Co-Authored-By`) or any similar attribution tags to commit messages.
3. **One commit per task.** When given multiple tasks in a single request, commit each task separately.

## Verification
4. Do **not** run preview-style checks. Prioritize the project's existing checks instead: `build`, `clippy`, `fmt`, and the `e2e` tests.
8. **Run the `e2e` suite only once, at the very end, after all requested work is done** — not after each individual task. The e2e run is slow; during development rely on `build`, `clippy`, `fmt`, and unit tests, then run `bash tests/e2e.sh` a single time before finishing. (New e2e phases/assertions may still be *written* per task; just don't *execute* the suite until the end.)

## Language
5. Use **English** in all changes (code, comments, commit messages, docs).

## Feature Planning
6. Future feature ideas live in `planned_features.md`, **always in English**, using `[ ]` / `[x]` checkbox syntax. Whenever a "would be nice later" idea comes up, record it there; tick items off as they ship.
7. **Backlog items are numbered with stable `#N` ids** (in the "Future ideas" section of `planned_features.md`). When asked to do "planned_features #5", look up that id in the file. Ids are never renumbered or reused; a shipped item keeps its id and flips to `[x]` in place (with a short "shipped: ..." note); a new idea takes the next free number at the end of its category.

## Changelog
9. **Always update `CHANGELOG.md` for any user-facing change**, as part of the same task and commit that makes the change (not a separate follow-up). Add an entry under a `## [Unreleased]` section — create that section at the top (above the latest released version) if it does not exist — following the existing Keep a Changelog style: group under `### Security` / `### Added` / `### Changed` / `### Fixed` / `### Removed`, and write a **bold lead-in sentence** followed by a short explanation, matching the voice of the entries already in the file.
10. **What counts as user-facing:** behavior, config/flags/env vars, API/endpoints, security fixes, CLI, defaults, or anything an operator/user would notice. **Skip** purely internal changes: `planned_features.md` edits, CI/build-infra tweaks, test-only changes, and no-op refactors. If unsure whether a change is user-facing, add a changelog entry.
11. **Releases:** on a version bump, move the `## [Unreleased]` block to a new `## [x.y.z] - YYYY-MM-DD` heading (today's date) and bump the version in all three crate `Cargo.toml` files (`aperio-client`, `aperio-server`, `aperio-config`) plus `Cargo.lock`.
