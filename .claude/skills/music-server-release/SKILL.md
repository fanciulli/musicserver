---
name: music-server-release
description: >-
  Cut a coordinated release of the Music Server project across all four
  repositories (musicserver-backend, musicserver-admin-ui,
  musicserver-volumio-plugin and the musicserver umbrella/desktop repo). Bumps
  every repo to a single shared version, commits, tags `v<version>`, pushes, and
  triggers the Docker and Tauri desktop builds. Use when the user asks to
  "release Music Server", "cut a new version", "tag a release", or runs
  /music-server-release.
---

# Music Server — coordinated release

Cuts a single, coordinated release across the four Music Server repositories.
All repos move to **one shared version** (`vX.Y.Z`) per release.

## Repositories and what each produces

| Repo | Local dir | Artifact on release |
| --- | --- | --- |
| `fanciulli/musicserver-backend` | `musicserver-backend` | Docker image `ghcr.io/fanciulli/musicserver-backend:vX.Y.Z` (workflow `docker-image.yml`, `workflow_dispatch`) |
| `fanciulli/musicserver-admin-ui` | `musicserver-admin-ui` | Docker image `ghcr.io/fanciulli/musicserver-admin-ui:vX.Y.Z` (workflow `docker-image.yml`, `workflow_dispatch`) |
| `fanciulli/musicserver-volumio-plugin` | `musicserver-volumio-plugin` | Source tag only (no CI) |
| `fanciulli/musicserver` (umbrella) | `musicserver` | Tauri desktop installers + GitHub Release (workflow `build-packages.yml`, triggers automatically on `v*` tag push) |

## Version files to bump (set ALL to the new version)

- `musicserver-backend/package.json` → `.version`
- `musicserver-admin-ui/package.json` → `.version`
- `musicserver-volumio-plugin/package.json` → `.version`
  - Also prepend a line to `.volumio_info.changelog` (e.g. `vX.Y.Z <summary>`).
- `musicserver` (umbrella) desktop packaging under `packaging/tauri/`:
  - `backend/package.json` → `.version`
  - `frontend/package.json` → `.version`
  - `backend/src-tauri/tauri.conf.json` → `.version`
  - `frontend/src-tauri/tauri.conf.json` → `.version`
  - `backend/src-tauri/Cargo.toml` → `version = "..."` (package section; crate name `musicserver-backend`)
  - `frontend/src-tauri/Cargo.toml` → `version = "..."` (package section; crate name `musicserver-frontend`)
  - After editing the Cargo.toml files you may refresh each lockfile
    (`cargo update --manifest-path <dir>/Cargo.toml -p musicserver-backend --precise <ver>`,
    likewise `musicserver-frontend` for the frontend) if `cargo` is available.
    **Note: both `Cargo.lock` files are git-ignored in this repo**, so they are
    never committed and are regenerated at build time — the refresh is optional
    and has no effect on the release commit. If Rust isn't installed, skip it.

> Historically the versions diverged (backend `0.0.1`, admin-ui `1.2.2`,
> volumio `0.1.1`, desktop `0.1.0`). From the first release driven by this skill
> they all converge onto the shared version. Don't try to "preserve" old numbers.

## Tooling

**First, detect which GitHub tooling is available** (`command -v gh`) and pick one
path for the whole run — don't mix:
- If `gh` is present, use the `gh` CLI (`gh workflow run`, `gh release create`,
  `gh run list`).
- If `gh` is **absent**, use the GitHub MCP tools (`mcp__github__*`) instead — this
  is the expected path in remote/web sessions, where `gh` is usually not
  installed. The `gh` commands shown below are illustrative; the MCP equivalent is
  noted at each step (`actions_run_trigger` for dispatch, `list_releases` /
  `get_release_by_tag` to check, and the create/update release tools for notes).

Git operations (bump/commit/tag/push) always use plain `git` regardless.

## Procedure

### 1. Confirm the new version (ASK the user)
1. Read the current `.version` from each `package.json` and report them.
2. Propose the next shared version. Ask the user to confirm or supply the target
   (accept either an explicit `X.Y.Z` or `major`/`minor`/`patch`). **Always ask
   — never pick the number silently.** Normalize to `X.Y.Z`; the git tag is
   `vX.Y.Z`.
3. Refuse to proceed if a tag `vX.Y.Z` already exists in any repo (check
   `git tag -l vX.Y.Z` / remote tags). Offer to pick a different number.

### 2. Pre-flight, per repo
For each of the four repos:
- `git fetch origin` (retry up to 4× with exponential backoff on network error).
- Releases are cut from **`main`**. Check out `main` and `git pull origin main`.
- Verify a clean working tree (`git status --porcelain` empty). If dirty, stop
  and report — do not stash or discard the user's changes.

### 3. Bump, commit, tag, push — per repo
For each repo, in this order: backend, admin-ui, volumio-plugin, musicserver.
1. Edit every version file listed above for that repo to the new version.
2. `git add -A && git commit -m "Release vX.Y.Z"`. Use exactly this single-line
   message — **no co-authoring trailers, no session/tool attribution** (see
   guardrails).
3. Create an **annotated** tag: `git tag -a vX.Y.Z -m "Release vX.Y.Z"`.
4. Push branch then tag, each with retry/backoff:
   - `git push -u origin main`
   - `git push origin vX.Y.Z`

> Pushing the `vX.Y.Z` tag to `musicserver` (umbrella) **automatically** starts
> `build-packages.yml`, which builds the Tauri installers and publishes the
> GitHub Release with generated notes. Do not dispatch it manually — in step 5
> you augment that release with the computed changelog instead of creating it.

### 4. Trigger the Docker image builds
The backend and admin-ui Docker workflows are `workflow_dispatch` and resolve
their image tag from the tag pointing at the dispatched ref, so dispatch them
**against the new tag**:

```bash
gh workflow run docker-image.yml --repo fanciulli/musicserver-backend  --ref vX.Y.Z
gh workflow run docker-image.yml --repo fanciulli/musicserver-admin-ui --ref vX.Y.Z
```

If `gh` is absent (the usual case in remote/web sessions), dispatch via MCP
instead: `actions_run_trigger` with `owner=fanciulli`, `repo=musicserver-backend`
(then `musicserver-admin-ui`), `workflow_file=docker-image.yml`, `ref=vX.Y.Z`.
These publish `ghcr.io/fanciulli/<repo>:vX.Y.Z`.

### 5. Compute the changelog and publish a GitHub Release per repo
For **each** repo, compute a changelog from the commit history and attach it to a
GitHub Release on the `vX.Y.Z` tag.

1. **Determine the previous tag** for the repo:
   `prev=$(git -C <repo> describe --tags --abbrev=0 "vX.Y.Z^" 2>/dev/null)`.
   If there is no previous tag (first release), **do not dump the entire history**
   — that pulls in unrelated bootstrap/skill/`chore` commits and makes a noisy
   changelog. Instead:
   - If the repo has *any* earlier tag at all, use the most recent one as the
     baseline even if it's a pre-release.
   - Otherwise (truly the first tag ever, e.g. the umbrella repo), ask the user
     for a baseline ref/date, or default to a curated short list of the
     user-facing changes rather than the raw `git log`. Drop release-bump and
     tooling/`chore`/skill commits from the output.
2. **Compute the changelog** from the commits in the range, excluding the
   release-bump commit itself:
   ```bash
   range="${prev:+$prev..}vX.Y.Z"            # "prev..vX.Y.Z" or just "vX.Y.Z"
   git -C <repo> log "$range" --no-merges \
     --pretty=format:'- %s (%h)' \
     | grep -vE '^- (Release v|music-server-release:|Add /music-server-release)' \
     > /tmp/changelog-<repo>.md
   ```
   Extend the `grep -vE` filter to drop other pure-tooling/`chore` subjects when
   the baseline is wide (see step 1).
   If the range is empty, write a single `- No changes` line. Prefer a readable
   list of `- <subject> (<short-sha>)`; you may group by type if the subjects use
   conventional-commit prefixes. Title the body with a `## vX.Y.Z` heading.
3. **Create / update the GitHub Release**, including the computed changelog as the
   body. Use the `vX.Y.Z` tag (already pushed):
   - backend, admin-ui, volumio-plugin (no auto-release exists, so create):
     ```bash
     gh release create vX.Y.Z --repo fanciulli/<repo> \
       --title "vX.Y.Z" --notes-file /tmp/changelog-<repo>.md
     ```
     If a release already exists, edit it instead:
     `gh release edit vX.Y.Z --repo fanciulli/<repo> --notes-file /tmp/changelog-<repo>.md`.
   - **musicserver (umbrella)**: `build-packages.yml` already creates this release
     (with installers + auto-generated notes). Do **not** create a second one —
     wait until that release exists, then set its body to include the computed
     changelog:
     ```bash
     gh release edit vX.Y.Z --repo fanciulli/musicserver --notes-file /tmp/changelog-musicserver.md
     ```
     (Optionally keep the auto-generated notes by appending them after the
     changelog rather than overwriting.)

(MCP equivalent: use the GitHub release tools to create the release with `tag_name=vX.Y.Z`
and `body=<changelog>`, or update the existing umbrella release's body.)

### 6. Verify and report
- Wait for / poll the workflow runs (`gh run list --repo <repo> --branch vX.Y.Z`
  or per workflow). Surface failures with their logs.
- Confirm a GitHub Release exists on `vX.Y.Z` for **each** repo and that its body
  contains the computed changelog.
- Confirm the umbrella release has the desktop installers (`.dmg`, `.deb`,
  `-setup.exe`) attached.
- Confirm both GHCR images exist at `:vX.Y.Z`.
- Report a concise summary: version, the four tags pushed, image tags published,
  the four release URLs, and the status of each triggered workflow.

## Guardrails
- This skill pushes tags, triggers builds and publishes images/GitHub Releases —
  these are outward-facing. Confirm the version with the user before pushing
  anything. Show the user the computed changelog before publishing it if unsure.
- Never force-push or delete existing tags/releases unless the user explicitly
  asks.
- If any repo fails mid-way (e.g. push rejected), stop and report which repos
  were already tagged/pushed so the release can be reconciled, rather than
  leaving a partial state silently.
- Keep commit/tag messages clean: a plain `Release vX.Y.Z` only. They must **not**
  contain any co-authoring notice (no `Co-Authored-By:` lines), session/tool
  attribution trailers, or internal model identifiers.
