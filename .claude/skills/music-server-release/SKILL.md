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
  - `backend/src-tauri/Cargo.toml` → `version = "..."` (package section)
  - `frontend/src-tauri/Cargo.toml` → `version = "..."` (package section)
  - After editing the Cargo.toml files, refresh `Cargo.lock` (run `cargo update -p music-server-backend -p music-server-admin-ui --precise <ver>` if `cargo` is available, otherwise `cargo build` in each `src-tauri` dir; if Rust isn't installed, leave the lockfiles and note it).

> Historically the versions diverged (backend `0.0.1`, admin-ui `1.2.2`,
> volumio `0.1.1`, desktop `0.1.0`). From the first release driven by this skill
> they all converge onto the shared version. Don't try to "preserve" old numbers.

## Tooling

Use the GitHub tooling available in the session:
- Prefer the `gh` CLI (`gh workflow run`, `gh release create`, `gh run list`).
- If `gh` is unavailable but GitHub MCP tools are (`mcp__github__*`), use
  `actions_run_trigger` for workflow dispatch and `list_releases`/`get_latest_release`
  plus the appropriate release tool instead.

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
2. `git add -A && git commit -m "Release vX.Y.Z"`.
3. Create an **annotated** tag: `git tag -a vX.Y.Z -m "Release vX.Y.Z"`.
4. Push branch then tag, each with retry/backoff:
   - `git push -u origin main`
   - `git push origin vX.Y.Z`

> Pushing the `vX.Y.Z` tag to `musicserver` (umbrella) **automatically** starts
> `build-packages.yml`, which builds the Tauri installers and publishes the
> GitHub Release with generated notes. Do not dispatch it manually.

### 4. Trigger the Docker image builds
The backend and admin-ui Docker workflows are `workflow_dispatch` and resolve
their image tag from the tag pointing at the dispatched ref, so dispatch them
**against the new tag**:

```bash
gh workflow run docker-image.yml --repo fanciulli/musicserver-backend  --ref vX.Y.Z
gh workflow run docker-image.yml --repo fanciulli/musicserver-admin-ui --ref vX.Y.Z
```

(MCP equivalent: `actions_run_trigger` with `workflow=docker-image.yml`,
`ref=vX.Y.Z`.) These publish `ghcr.io/fanciulli/<repo>:vX.Y.Z`.

### 5. Volumio plugin release notes (optional)
The volumio plugin has no CI. After pushing its tag, optionally create a GitHub
Release for it (`gh release create vX.Y.Z --repo fanciulli/musicserver-volumio-plugin --generate-notes`)
so users can download the tagged source.

### 6. Verify and report
- Wait for / poll the workflow runs (`gh run list --repo <repo> --branch vX.Y.Z`
  or per workflow). Surface failures with their logs.
- Confirm the umbrella GitHub Release was created and the desktop installers
  (`.dmg`, `.deb`, `-setup.exe`) are attached.
- Confirm both GHCR images exist at `:vX.Y.Z`.
- Report a concise summary: version, the four tags pushed, image tags published,
  release URL, and the status of each triggered workflow.

## Guardrails
- This skill pushes tags, triggers builds and publishes images/releases — these
  are outward-facing. Confirm the version with the user before pushing anything.
- Never force-push or delete existing tags/releases unless the user explicitly
  asks.
- If any repo fails mid-way (e.g. push rejected), stop and report which repos
  were already tagged/pushed so the release can be reconciled, rather than
  leaving a partial state silently.
- Keep commit/tag messages free of any internal model identifiers.
