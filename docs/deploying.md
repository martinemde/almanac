# Deploying Almanac

This runs the broker and `almanac-renovate` as systemd user services on one host, with PostgreSQL and rootless Podman. Each stage has a step to verify it and a way to stop.

## What you need

- A host with Podman, PostgreSQL, the `claude` CLI, and a user with linger enabled: `loginctl enable-linger $USER`. Linger keeps user services running with no login session, which rootless Podman needs.
- A public HTTPS ingress that forwards to `127.0.0.1:8787`. The broker listens only on loopback. Any reverse proxy or tunnel works, and the signature covers the body, so a path rewrite is safe.
- An `ANTHROPIC_API_KEY`. The harness runs in `--bare` mode, which takes only an API key, not a subscription login.

## 1. Build

```bash
make package
podman build -t localhost/almanac-fixer:latest images/polyglot
```

`make package` puts `almanac-broker` and `almanac-renovate` in `dist/`. Copy both to `~/.local/bin/`.

The image holds Node, pnpm, Go, and bun, because a sandbox has no network. Add a repository's other toolchains to it.

## 2. Create the GitHub App

One App, one webhook URL, for every repository. Personal accounts have no account-level webhook.

**Webhook:** set the URL to your ingress plus `/webhooks/github`, and a random secret. Subscribe to `check_suite`, `pull_request`, and `status`.

**Repository permissions**, each no broader than it must be:

| Permission | Access | Why |
|---|---|---|
| Metadata | Read | Required by GitHub |
| Pull requests | Read | Reads state, labels, and changed files |
| Checks | Read | Reads check suites and runs |
| Actions | Read | Reads failed job logs |
| Commit statuses | Write | Posts `almanac/trust` |
| Contents | Write | Reads the source and pushes a checked patch |
| Issues | Write | Files questions for a person |

**Do not grant Workflows.** Without it, GitHub itself refuses a push that changes `.github/workflows`, a second guard behind the patch policy.

Generate a private key, and install the App on **one repository** for now.

## 3. Configure

Create a database, then two environment files readable only by you:

```bash
createdb almanac
install -m 600 contrib/broker.env.example ~/.config/almanac/broker.env
install -m 600 contrib/almanac-renovate.env.example ~/.config/almanac/almanac-renovate.env
```

Fill them in. Secrets belong there and never in `almanac.toml`. Set `ALMANAC_BOT_LOGIN` to your App's bot login, which is its slug followed by `[bot]`, so Almanac recognises its own echoes.

## 4. Configure Renovate

Add to your Renovate configuration:

```json
{
  "gitIgnoredAuthors": ["almanac[bot]@users.noreply.github.com"],
  "vulnerabilityAlerts": { "labels": ["security"] }
}
```

- `gitIgnoredAuthors` matches a commit's author email. Without it, Renovate treats a patched branch as edited and will not automerge it. Change the value if you set a different commit author.
- A patch survives until a conflict. With `rebaseWhen: "conflicted"`, Renovate rewrites a conflicted branch from the base branch and drops the patch. Almanac sees the new commit and tries again, up to its attempt cap.
- For `almanac/trust` to hold a merge, set `platformAutomerge: false`, or make it a required check. A platform automerge ignores any status that is not required, and `report` shows those repositories as `blocked-by-config`.

## 5. Start the broker

```bash
cp contrib/systemd/almanac-broker.service contrib/systemd/almanac@.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now almanac-broker
```

**Verify:** `curl -f http://127.0.0.1:8787/readyz`, then use **Recent Deliveries** on the App to send a ping. A signed delivery gets `202`.

**Stop:** `systemctl --user stop almanac-broker`. GitHub records the failures, and catch-up scans cover them.

Expect some failed deliveries around a change to your ingress. Scans recover what they miss.

## 6. Dry run on one repository

A run is dry unless `ALMANAC_MODE=live` is set, and a read-only pass cannot write or call a model. Both commands read `almanac-renovate.env`, which systemd loads for the service and a shell does not:

```bash
set -a; . ~/.config/almanac/almanac-renovate.env; set +a
ALMANAC_READ_ONLY=1 almanac-renovate report
ALMANAC_READ_ONLY=1 almanac-renovate backfill --repo owner/repo --dry-run
```

**Verify:** every open Renovate pull request has a state. A run that stops with `an agent would run here` is what a repair looks like at this stage.

**Stop:** nothing is running.

## 7. Go live on one repository

Set `ALMANAC_MODE=live` in the environment file, then:

```bash
systemctl --user enable --now almanac@almanac-renovate
```

**Verify:** `curl -f http://127.0.0.1:8788/healthz`, and `journalctl --user -u almanac@almanac-renovate`. Watch one repair: `almanac/trust` goes `pending`, a commit lands on the pull request's branch, and the status becomes `success` once CI passes.

**Stop:** `systemctl --user stop almanac@almanac-renovate`. No new run starts, an in-flight run gets 30 seconds, and anything unfinished resumes on the next start. To stop writes while staying up, remove `ALMANAC_MODE=live` and restart.

## 8. Widen

Install the App on the remaining repositories. Nothing else changes, because the broker needs no change for a new repository or workflow.

**Verify:** `almanac-renovate report`. Every pull request is `mergeable` or names who or what it is waiting on.

**Stop:** uninstall the App from a repository to stop all activity there.

## Living with it

- A pushed patch runs in CI, which may hold secrets. The patch policy denies CI and toolchain files, but a patch can still change a test or a package script that CI runs. Check that branch-triggered workflows hold only what they need.
- A security update with broken CI waits out the cooldown before it is repaired, up to 72 hours.
- An abandoned sandbox ends itself within two hours.
- During a long database outage at boot, `TimeoutStartSec=infinity` keeps the unit waiting instead of restarting it.
- Errors have codes: `almanac-renovate explain almanac::store::failed`.

## Application container (report proof)

```bash
podman build -t localhost/almanac:report -f Containerfile .
python3 tools/test-container.py localhost/almanac:report
podman run --rm --read-only --env-file report.env localhost/almanac:report
```

`report.env` supplies `ALMANAC_OWNER` and a read-only `GITHUB_TOKEN` (or the `GITHUB_APP_` settings); keep it outside version control. No model key or database is needed for `report`. The image runs as numeric user 65532. Its default entrypoint is `almanac-renovate`, its default command is `report`, and it sets read-only/dry mode. Use a published digest rather than the mutable `main` tag for a repeatable run.

The broker binary is also included. It requires `DATABASE_URL` and `ALMANAC_WEBHOOK_SECRET` and enforces a loopback listener. On Linux, an explicit broker run can use `podman run --rm --network=host --env-file broker.env --entrypoint almanac-broker localhost/almanac:report`; publish no container ports and retain the loopback-only ingress arrangement above. This starts the broker only when explicitly requested; the image never starts a daemon by default.

This image cannot run repairs: those need the external `claude` and `podman` subprocesses plus a prepared sandbox image and caches. Do not treat it as the full host deployment described above. See [the container build decision](adrs/container-build.md) for the proof's scope and CI publication behavior.
