---
name: land
description: >-
  Land explicitly requested lark2tapd changes into the original local repository's main branch, with verification and no external publication. Invoke only when the user requests landing or merging the relevant change, including an explicit /land invocation; never for review, preparation, passing checks, or skill installation alone.
disable-model-invocation: true
metadata:
  delta-action: land
---

# Land lark2tapd changes locally

## Scope and authorization

This workflow applies only to the lark2tapd Rust/macOS application identified by the root `Cargo.toml`, `crates/desktop/Cargo.toml`, and `scripts/package-macos.sh`.

An explicit invocation of this skill or an equivalent explicit landing request authorizes carrying out this workflow. Do not ask again whether the user wants to merge. Installation or review approval alone does not authorize execution. Stop for a genuine blocker, ambiguous change scope, or a new action outside the approved boundaries.

The approved destination is the original local repository at `/Users/roxi/other_projects/lark2tapd`, branch `main`. The user explicitly permits direct local Git operations in that checkout. Verify this repository and the source repository before every run; if their locations or identities differ from this scope, stop rather than choosing another destination.

- Land the complete requested change, not merely the last commit. For the first delivery this includes the first-stage application, architecture-audit fixes, and this landing skill.
- Preserve the source development branch and all existing commits. Prefer fast-forward integration; do not squash or rewrite history.
- Resolve merge conflicts automatically only when the intended result is clear. Preserve unrelated work. Pause and report conflicts whose correct resolution requires a product decision or is otherwise ambiguous.
- Do not push to an external remote, open a pull request, publish a package, change branch protections or Git receive policy, or use force pushes.
- Do not automatically stash, reset, clean, remove worktrees, delete branches, or perform post-landing cleanup.
- Do not install CLIs, access real Keychain credentials, authorize OAuth, or invoke real Lark/TAPD business operations during verification. Tests use their existing isolated substitutes.

## 1. Establish source, destination, and applicable requirements

Use non-interactive Git commands. Prefix every Git command that may launch an editor with `GIT_EDITOR=true`; supply explicit commit messages and retain normal hooks and signing requirements. Do not use `--no-verify`, signing bypasses, interactive rebases, or a pager.

From the attached source checkout, determine its actual root and current branch, rather than reusing a path from a previous session:

```sh
source_root=$(git rev-parse --show-toplevel)
target_root=/Users/roxi/other_projects/lark2tapd
source_branch=$(git branch --show-current)
git --no-optional-locks status --short --branch
git log -6 --oneline
git branch -a
git remote
```

Require the resolved source checkout to differ from the original destination checkout and the source to be on a named development branch other than `main`. Otherwise stop and request a separate source checkout rather than preparing commits directly on the shared destination. Keep the captured roots, branches, and commit IDs explicitly available in every shell invocation; shell-local variables do not automatically persist between commands executed in different shells.

Inspect the relevant remote URLs without printing embedded credentials; redact userinfo and secret query parameters before displaying any URL. Inspect only relevant non-secret configuration, never the entire environment or credential files. The researched repository has only a `local` backlink; that name is not an external publication remote or proof of destination identity.

Check the original checkout with read-only operations:

```sh
git --no-optional-locks -C "$target_root" status --porcelain=v1 --untracked-files=all
git -C "$target_root" branch --show-current
git -C "$target_root" rev-parse --show-toplevel
git -C "$target_root" rev-parse refs/heads/main
git -C "$target_root" remote
git -C "$target_root" config --get core.hooksPath
git -C "$target_root" config --get commit.gpgsign
```

An absent optional configuration value is not a failure. Require the original checkout to be on `main`, with no staged, unstaged, or untracked user changes and no merge/rebase/cherry-pick in progress. If it is not, stop; do not switch its branch or hide its changes. Preserve ignored local files too: use `--no-overwrite-ignore` for merges.

Read current applicable `AGENT.md`/`AGENTS.md`, contribution policies, submission templates, Git attributes/hooks, verification scripts, and CI definitions. Recheck policy before landing if the candidate changes any of these. At setup there were no repository instruction files, contribution templates, active hooks, CI workflows, external remotes, or mandatory signing configuration. These observations are not permanent exemptions: honor new applicable requirements, including signing, contributor agreements, reviews, human-authored submission text, and conditional documentation/test requirements. Do not manufacture a hosting-service check or weaken policy because it is not enforced by a service. If a new required external check cannot be satisfied without prohibited publication, stop and explain the conflict.

Check macOS and the availability of Git, a Rust toolchain supporting edition 2024, rustfmt, Clippy, Xcode, `sqlite3`, `plutil`, and `codesign`. Do not install missing prerequisites. The tests use temporary SQLite databases and fake CLIs; the source defines the executable as macOS-only. Sources: `Cargo.toml`, `crates/desktop/Cargo.toml`, `crates/desktop/src/main.rs:1-3`, `crates/desktop/src/worker_settings_tests.rs`, and `scripts/package-macos.sh:5-38`.

## 2. Prepare the requested change without including unrelated work

Inspect staged, unstaged, and untracked changes in the source checkout. Commit only files that belong to the requested delivery, including a newly installed skill when it is part of this request. If ownership or scope is unclear, ask before including them. Never stage everything indiscriminately, discard unrelated files, or silently omit requested changes.

Use an explicit Conventional Commit message, such as `chore: configure local landing workflow` for this skill alone. Preserve applicable signing and hooks. A failure is a blocker, not a reason to bypass them.

Before creating preparation commits, perform the checks appropriate to those edits: skill/documentation-only preparation requires frontmatter/reference validation and `git diff --check`; implementation changes require the relevant verification from section 4. Preparation checks do not replace verification of the final integrated candidate.

After the requested work is committed, require a clean source checkout, record the exact source commit as `source_tip`, and keep the original source branch intact:

```sh
source_tip=$(git rev-parse HEAD)
```

Do not assume that a prior review or an earlier successful test run covers this exact commit.

## 3. Build a candidate against the original main

Read and record the destination's exact `main` commit as `target_before`. Fetch only from the verified local repository into the attached source repository; do not fetch or publish to an external service:

```sh
target_before=$(git -C "$target_root" rev-parse refs/heads/main)
git fetch --no-tags "$target_root" refs/heads/main
fetched_target=$(git rev-parse FETCH_HEAD)
```

Require `fetched_target` to equal `target_before`; otherwise restart the preflight against the new destination state. Verify that the source and destination have related histories. Do not use `--allow-unrelated-histories`.

Inspect the complete branch difference, commit history, and any applicable architecture-review findings. For the first-stage delivery, read `docs/reviews/phase-one-architecture-review.md`, including its unresolved limits. Do not portray known limits or unperformed real-account testing as fixed or verified.

- If `target_before` is an ancestor of `source_tip`, use `source_tip` as the candidate. This preserves all source commits and permits a destination fast-forward.
- If the requested source is already an ancestor of `target_before`, verify that all requested changes are present at the destination and report that they are already landed; do not create an empty merge.
- If the histories diverged, create a uniquely named local integration branch, such as `chore/user/land-main-<unique-suffix>`, in the attached checkout from `target_before`. Never reuse or overwrite an occupied branch. Merge the captured `source_tip` into this branch with an explicit Conventional Commit merge message:

```sh
GIT_EDITOR=true git merge --no-ff --no-edit --no-autostash --no-overwrite-ignore \
  -m "chore: merge verified lark2tapd changes into main" "$source_tip"
```

For the unique suffix, compute a concrete unused value at execution time; do not run the example placeholder literally. Resolve only unambiguous conflicts, using the task requirements and both histories. Do not take an entire side merely to make a merge succeed. If intent is ambiguous, leave the original repository unchanged, report the conflict, and wait. Do not automatically abort or delete the integration work.

After conflict resolution, review the resulting diff, stage only the resolved files, and complete the merge using an explicit non-interactive commit message and the repository's normal hooks/signing. Record the final candidate commit as `candidate`. Keep the integration branch and development branch; cleanup is outside this workflow.

## 4. Verify the exact candidate before landing

Run checks from the candidate's clean checkout. All required checks must finish successfully for the exact candidate tree. A started, pending, failing, missing, or unverifiable required check is not success. If code or conflict resolutions change after a check, rerun affected checks and the required final suite before landing. Do not update lockfiles to get past `--locked` failures without an approved change and fresh verification.

For this first-stage delivery, or any delivery changing Rust code, manifests, lockfiles, editor patches, tests, build scripts, or application assets, run all of:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked --quiet
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Sources: root `Cargo.toml:1-3` defines the three workspace members and excludes the vendored editor; `Cargo.toml:25-26` applies the editor patch; the crate manifests define the actual library/binary and test-support dependencies. These are standard Cargo/rustfmt/Clippy invocations, whose arguments were checked against installed tool help during setup. Do not substitute a filtered test run for the full workspace requirement.

Run the vendored library tests separately because it is excluded from the workspace:

```sh
cargo test --locked --manifest-path vendor/zorite-editor/Cargo.toml --lib --target-dir target --quiet
```

Sources: `Cargo.toml:3` and `vendor/zorite-editor/Cargo.toml:40-42,72-75` define the excluded library target and test support. Keep `--lib`: the unused upstream demo example is not the application's integration target and has different dependency assumptions.

Build and verify the local application bundle:

```sh
sh scripts/package-macos.sh
```

Source: `scripts/package-macos.sh:6-12,13-38` implements the no-argument debug build, bundle construction, plist validation, ad-hoc signing, and strict signature verification. This does not publish, notarize, or install the application. Do not open the resulting application for an automated real-account smoke test.

Check the non-business startup path:

```sh
env -u LARK2TAPD_MOCK cargo run --locked -p lark2tapd -- --help
```

Sources: `crates/desktop/Cargo.toml:1-5` names the executable package; `crates/desktop/src/startup.rs:9-46` accepts `--help` and rejects nonempty legacy mock variables; `crates/desktop/src/main.rs:16-46` prints help and returns before worker/credential initialization. Do not substitute normal GUI startup or removed mock flags.

Finally run:

```sh
git diff --check
git --no-optional-locks status --porcelain=v1 --untracked-files=all
```

Require no unexpected tracked, staged, or untracked changes, no conflict markers, and the same candidate commit. Generated ignored build output is allowed. The existing `block v0.1.6` future-compatibility warning must be reported accurately; do not suppress new warnings or bypass strict Clippy.

For a later delivery containing only documentation or skill instructions, inspect the full diff relative to `target_before`, validate changed frontmatter and every referenced path/command, and run `git diff --check`. The Rust/build suite may be omitted only when no executable code, build input, asset, manifest, lockfile, or test changed and no applicable policy requires it; state this scope explicitly. This exemption does not apply to the first-stage application delivery merely because its last commit only adds this skill.

Any applicable required review or CI introduced after setup must also be verified for the candidate before landing. Automated local tests do not certify real OAuth, Keychain, TAPD writes/uploads, macOS system dialogs, or untested manual acceptance criteria. If the user makes a manual check a landing condition, wait for its result rather than substituting fake-backend tests.

## 5. Land into the original checkout

Immediately before changing the destination, recheck its root, checked-out branch, exact `main` commit, clean index/worktree, untracked files, and absence of an in-progress Git operation. Require `main == target_before`. If the destination advanced or became dirty, stop or rebuild and reverify a candidate against the new clean target; never overwrite concurrent user work.

Transfer the already verified candidate's Git objects locally, without updating a destination branch as part of the transfer:

```sh
git -C "$target_root" fetch --no-tags "$source_root" "$candidate"
```

Verify that the fetched commit equals the captured `candidate`, then recheck the destination preconditions. Merge the immutable commit ID, not a mutable source branch or `FETCH_HEAD` that another process could replace:

```sh
GIT_EDITOR=true git -C "$target_root" merge --ff-only --no-autostash --no-overwrite-ignore "$candidate"
```

This is the actual landing operation. The original checkout remains on `main`; all conflict resolution happened in the attached source/integration checkout. If fast-forward is rejected, report that the change has not landed and inspect the blocker. Do not change `receive.denyCurrentBranch`, force-update refs, retry with destructive flags, or perform an unverified merge in the destination.

## 6. Verify and report the destination

Read back the original repository after the merge:

```sh
git -C "$target_root" branch --show-current
git -C "$target_root" rev-parse HEAD refs/heads/main
git -C "$target_root" rev-parse 'HEAD^{tree}'
git --no-optional-locks -C "$target_root" status --porcelain=v1 --untracked-files=all
git -C "$target_root" merge-base --is-ancestor "$source_tip" refs/heads/main
```

Require the branch to be `main`, `HEAD` and `refs/heads/main` to equal the verified `candidate`, the tree to equal the verified candidate tree, the source tip to be included, and the checkout to be clean. Never report source-branch publication, a prepared merge commit, or running tests as successful landing. If post-landing verification fails, state that the mutation occurred but final verification failed; do not automatically roll back or claim success.

Report the destination, landed commit, included source, actual checks and results, any deliberately untested/manual boundaries, and the fact that no external publication or cleanup occurred. Leave the source development branch, any integration branch, and worktrees intact.
