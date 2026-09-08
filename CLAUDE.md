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

Both branches deploy the **same manifests** — byte-identical, so a `dev` -> `master` merge
never conflicts. There are no per-environment name suffixes: the environments are different
namespaces.

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
Nothing is provisioned. `k8s/redis.yaml` is a single-pod Redis, commented out of the
kustomization — uncomment it and ask the admin for its password Secret. Treat it as
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
- **Resource budget** per namespace: 10 pods, 1 CPU / 2Gi requested, 2 CPU / 4Gi limit,
  3 PVCs. Nodes are 4-core with 1GbE between them.

## Static content

Content lives in this repo and is baked into the image by the Dockerfile, so it is
versioned with the code and a deploy is one new tag. There is no bucket to upload to —
Garage is for application data, not site content.

## Checking on things

Cluster access is the admin's, not this repo's — there is no kubeconfig here and the API is
not reachable from outside the house. What this repo can see is CI: whether the build passed
and whether the tag-bump commit landed. If a deploy does not appear, ask the cluster admin to
check the Flux Kustomization for this namespace; a failed one names the offending manifest.
