---
name: troubleshoot
description: Diagnose a failing deploy or a misbehaving app on the Tamarin k3s cluster. Use when something does not come up, a connection hangs, an image will not pull, or a deploy appears to do nothing.
---

# Troubleshooting

The failure modes here are specific, and several of them look like application bugs when
they are not. Check these before changing application code. Cluster access belongs to the
admin — what you can see from this repo is CI and the manifests.

## The deploy did nothing

1. Did CI pass? `gh run list --limit 3`. If the build failed there is no new image and no
   tag-bump commit, so there is nothing for Flux to apply.
2. Did the tag-bump commit land? `git log --oneline -3` after pulling. The workflow commits
   `deploy ghcr.io/...` back to the branch; if that commit is missing, the workflow's push
   failed and the cluster is still on the old image.
3. Otherwise ask the admin to check the Flux Kustomization for this namespace. `wait: true`
   means a bad manifest fails the whole set, and the error names the offending object.

## A connection hangs and never errors

**Almost always the NetworkPolicy, not your code.** Egress is default-deny. This app may
reach cluster DNS, `pg-rw.postgres.svc.cluster.local:5432`, Garage on `:3900`, its own
namespace, and the public internet. Everything else is dropped silently, which presents as a
hang rather than a refusal. The Kubernetes API, other apps and the 192.168.2.0/24 LAN are
blocked deliberately.

## The pod will not start

- `ImagePullBackOff` with a 401: the GHCR package is private and the namespace has no
  `ghcr-creds` Secret. Making the package public is the simpler fix.
- Rejected at creation, with an error that reads like a manifest syntax problem: Pod
  Security `baseline`. Anything `privileged`, `hostPath`, `hostNetwork`, `hostPID` or a
  host port is refused outright.
- `CreateContainerConfigError`: an env var references a Secret key that does not exist in
  this namespace.
- `Pending` forever with a PVC: `local-path` pins a pod to the node its volume was created
  on. If that node is unavailable the pod waits rather than moving.

## Redis says WRONGPASS and the password is right

The URL is `redis://:<password>@...`, which sends an *empty username*. A server using
`requirepass` has only the `default` user, so it rejects the empty one. Write
`redis://default:<password>@monke-app-redis:6379/0`.

## A crashloop with no useful logs

Ask for the **previous** container's logs (`kubectl logs --previous`) — without that flag
you see a container that has not failed yet.

## The site serves an old version

A moving tag. Every deploy must pin an immutable tag; CI does this, so this usually means
the tag was edited by hand.

## TLS is wrong or the certificate stopped renewing

Renaming the Ingress orphans its Certificate: the Certificate is owned by the Ingress that
requested it, so deleting one garbage-collects the Certificate while the TLS Secret
survives, and a new Ingress will not adopt it. The site then serves a valid certificate that
nothing is renewing. After a rename the admin has to delete the orphaned Secret and let
cert-manager reissue.
