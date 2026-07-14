# Project Rules

These rules apply to all future work in this repository.

## Git & Commits
1. **Never push.** Do not run `git push` under any circumstances.
2. **Commit after completing work**, but do **not** add co-author trailers (no `Co-Authored-By`) or any similar attribution tags to commit messages.
3. **One commit per task.** When given multiple tasks in a single request, commit each task separately.

## Verification
4. Do **not** run preview-style checks. Prioritize the project's existing checks instead: `build`, `clippy`, `fmt`, and the `e2e` tests.

## Language
5. Use **English** in all changes (code, comments, commit messages, docs).

## Feature Planning
6. Future feature ideas live in `planned_features.md`, **always in English**, using `[ ]` / `[x]` checkbox syntax. Whenever a "would be nice later" idea comes up, record it there; tick items off as they ship.
7. **Backlog items are numbered with stable `#N` ids** (in the "Future ideas" section of `planned_features.md`). When asked to do "planned_features #5", look up that id in the file. Ids are never renumbered or reused; a shipped item keeps its id and flips to `[x]` in place (with a short "shipped: ..." note); a new idea takes the next free number at the end of its category.
