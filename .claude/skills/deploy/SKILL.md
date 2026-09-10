---
name: deploy
description: Ship a change to the Tamarin k3s cluster. Use when asked to deploy, ship, release, push a new version, update the running app, or roll back.
---

# Deploying

A deploy is a push. Flux watches this repo and applies `k8s/`; nothing here talks to the
cluster, and there is no `kubectl apply` step to run.

| Branch | Lands in |
|---|---|
| `master` | `yarn-monke-app-prod` |
| `dev` | `yarn-monke-app-dev` |

## Shipping a change

```bash
git switch dev            # or master for production
git add -A && git commit -m "..."
git push
```

CI then does the rest: it builds the image, pushes it to `ghcr.io/monkecloud/art-monke:<sha>`,
and commits that exact tag into `k8s/`. Flux applies that second commit. **Never edit the
image tag by hand and never use `:latest`** — the tag is how rollback works, and a moving
tag means a restarted pod silently changes version.

Watch the build:

```bash
gh run list --limit 3
gh run view --log-failed
```

## Promoting dev to prod

Merge `dev` into `master` and push. The prod branch builds its own image and its own tag bump,
so the two environments never share a tag by accident.

Expect one conflict per service, on the `image:` line in `k8s/web.yaml` and `k8s/api.yaml` —
each branch's CI wrote its own tag there. Take `dev`'s in every case: it is the code being
promoted, and master's build overwrites both a minute later. Nothing else should ever
conflict; if it does, someone has put an environment difference on a branch, which belongs
in the cluster's dev overlay instead.

## Rolling back

```bash
git revert <commit>
git push
```

That is the whole procedure. Reverting the tag-bump commit alone is enough to go back to the
previous image, and it keeps the repo describing exactly what is running. Do not ask the
admin to `kubectl rollout undo` — Flux would put the broken version straight back on its next
reconcile.

## Before pushing

- The manifests are applied with `wait: true`, so a bad manifest fails the reconcile
  loudly rather than half-applying. Nothing is deployed until the whole set is valid.
- New env vars need a matching Secret key in the namespace. Adding one that does not exist
  leaves pods stuck in `CreateContainerConfigError` — ask the admin first.
- A new PVC will be refused: this app is stateless apart from the optional cache.
