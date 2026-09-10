# monke-app

This app runs on the Tamarin k3s cluster. It has a namespace of its own per environment
(`yarn-monke-app-prod`, `yarn-monke-app-dev`) and reaches the cluster's shared
Postgres and S3. Everything it runs is described by `k8s/` in this repo.

## Deploying is pushing

Flux watches this repo and applies `k8s/` — nothing is deployed by hand, and nothing
reaches into the cluster from outside.

- push to **`master`** → prod namespace
- push to **`dev`** → dev namespace

CI builds the image, pushes it to `ghcr.io/monkecloud/monke-app`, and commits the new tag
into `k8s/`. That commit is what Flux picks up. A rollback is `git revert` plus a push —
never an out-of-band change, or the repo stops describing what is running.

Both branches deploy the **same manifests**. There are no per-environment name suffixes:
the environments are different namespaces.

The one line that legitimately differs between the branches is the image tag, because each
branch's CI commits its own. A `dev` -> `master` merge therefore conflicts on exactly that
line whenever the two branches last built different code — take `dev`'s, since that is the
code you are promoting, and master's CI will rebuild and rewrite it anyway.

**Never introduce a branch difference in `k8s/`.** The two things that genuinely differ per
environment — the Postgres key (`uri` vs `uri_dev`) and the public hostname — are patched in
by the cluster's own overlay at apply time. If something else needs to differ per
environment, ask the cluster admin to add it there rather than editing one branch.

## What the cluster provides

Credentials arrive as **environment variables**, from Secrets the cluster admin puts in the
namespace. Never hardcode them, never commit them, never log them.

### Postgres 18 (shared cluster, 1 primary + 2 replicas)
- `DATABASE_URL` — `uri` in prod, `uri_dev` on the dev branch

Writes go to `pg-rw.postgres.svc.cluster.local`. `pg-ro` is the read-only replica endpoint —
fine for heavy reads, never assume it is current.

### S3 object storage (Garage)
- `S3_ENDPOINT`, `S3_BUCKET`, `S3_ACCESS_KEY`, `S3_SECRET_KEY`

Garage is S3-compatible but **not** AWS: pass the endpoint explicitly and use path-style
addressing (`aws --endpoint-url "$S3_ENDPOINT" s3 ...`, or `endpoint_url=` in boto3).

### A cache, if this repo wants one
`k8s/redis.yaml` is a single-pod Redis, and this repo runs it: it is in the kustomization
and its password Secret (`monke-app-redis`) is in the namespace. Treat it as
**disposable**: one pod on one node, unavailable while that node is down and gone for good
if the node is lost. Not backed up, not replicated.

Its URL needs `default` as the username: `redis://default:<password>@monke-app-redis:6379/0`.
The `redis://:<password>@...` form sends an empty username and the server answers
`WRONGPASS`, which reads like a wrong password.

## Hard constraints

- **Stay stateless.** No PersistentVolumeClaim except the optional cache above. Durable
  state belongs in Postgres or the bucket — those are the only two replicated stores.
- **Namespace-scoped only.** No ClusterRoles, CRDs, namespaces or PersistentVolumes. Flux
  applies this repo as a ServiceAccount that cannot create them, so such a manifest fails
  the whole reconcile rather than partly applying.
- **Egress is restricted.** Reachable: Postgres, Garage, cluster DNS, this namespace's own
  pods, and the public internet. Not reachable: the Kubernetes API, other apps, the house
  LAN. A blocked connection **hangs** rather than erroring — that is this, not a bug to
  route around.
- **Pod Security `baseline` is enforced.** No `privileged`, no `hostPath`, no
  `hostNetwork`/`hostPID`, no host ports. Warnings about the stricter `restricted` profile
  are advisory.
- **Resource budget** per namespace: 25 pods, 2 CPU / 4Gi requested, 4 CPU / 8Gi limit,
  3 PVCs. Nodes are 4-core with 1GbE between them. The shared default is lower (10 pods,
  1 CPU / 2Gi, 2 CPU / 4Gi); this namespace is raised above it by an overlay patch in the
  infra repo, for the transcode workers. Ephemeral storage is not in the quota at all, so a
  pod's `ephemeral-storage` request is per-pod hygiene rather than a quota negotiation.
  Budget a rolling update's surge pod when totalling limits: `web` and `api` each add one.

## Services

One directory per service under `apps/`, each with its own Dockerfile and its own GHCR
image, and each with a Deployment in `k8s/`:

- `apps/web` — TypeScript + Vite + React, built and served by nginx. `ghcr.io/monkecloud/monke-app`.
- `apps/api` — Rust + tokio + axum. `ghcr.io/monkecloud/monke-app/api`.
- `apps/worker` — Rust. Transcodes uploads into AAC tiers by consuming the `transcode_jobs`
  queue in Postgres. `ghcr.io/monkecloud/monke-app/worker`. No Service and no Ingress path:
  it is not web-facing, so it needs neither.

The two Rust services are one **Cargo workspace** rooted at the repo root, sharing
`apps/common` (Postgres/Garage/Redis wiring and the transcode target list). A path dependency
outside a Docker build context does not exist as far as the build is concerned, so both Rust
images build with the **repo root as their context** and name their Dockerfile explicitly;
`.dockerignore` at the root is what keeps that context from including every `target/` and
`node_modules/`. `apps/web` still builds from its own directory. `Cargo.lock` is at the root
and covers all three crates, and `[profile.*]` only takes effect there.

CI builds every service as a matrix and commits a single tag bump once all of them succeed,
so a half-built set never reaches Flux. Every service ships on the short SHA of the commit
that built it, so one tag describes the whole repo and a rollback stays one revert. Each
matrix entry carries its own `context` and `dockerfile`.

Traefik routes by path prefix on the one hostname — `/api` to the api, `/` to web. The
prefix is not stripped, so the api serves its routes under `/api`. A new service needs a
path here, not a new hostname: hostnames are patched per-environment by the cluster overlay
and adding one is an admin change.

## Static content

Content lives in this repo and is baked into each service's image by its Dockerfile, so it
is versioned with the code and a deploy is one new tag. There is no bucket to upload to —
Garage is for application data, not site content.

## Checking on things

Cluster access is the admin's, not this repo's — there is no kubeconfig here and the API is
not reachable from outside the house. What this repo can see is CI: whether the build passed
and whether the tag-bump commit landed. If a deploy does not appear, ask the cluster admin to
check the Flux Kustomization for this namespace; a failed one names the offending manifest.
