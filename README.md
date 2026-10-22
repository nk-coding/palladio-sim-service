# Palladio Architecture plugin service (`palladio-arch-service`)

An mdeo plugin service, written in Rust, that does one thing: it **simulates the plain Palladio
(PCM) part of a model with [SimOxide](../simoxide)** and returns what the simulation measured.
One script function, `pcmEvaluate`, runs one dynamic simulation (per-scenario load profile,
Kubernetes-style autoscaling through SPD and SimOxide's HPA) and returns raw measurements:
response-time statistics, utilisation and pods per processing resource, call counts and the
container sets of the sessions' synchronous paths, over a window and per time bin. Every
interpretation (cost, failure model, budgets, consistency, which percentile matters) belongs to
the mdeo scripts. It uses [palladio-sim-service](../palladio-sim-service) as a library
(protocol, auth, PCM schema, XMI writer). Used by the domain project
([palladio-architecture-optimization-domain](../palladio-architecture-optimization-domain)).

The analytic functions (`pcmResources`, `pcmScenarios`, `pcmOperations`, `pcmCallRates`,
`pcmWeightedVisits`, `pcmOperationVisits`, `pcmFailure`, `pcmSourceFailure`,
`pcmOperationFailure`, `pcmWarnings`), the StoEx evaluator `pcmMean`, the per-call simulation
functions (`pcmSimulate`, `pcmStat`, `pcmRun`, `pcmRunStat`, `pcmHpaRun`) and the layer-2
translation with its `oracle` and `translate` commands were removed on 2026-10-22; tag
`pre-simulation` has them (tag `pre-cleanup` the earlier `arch*` functions).

## Deployment

```bash
cargo build --release          # → target/release/palladio-arch-service
PORT=3022 target/release/palladio-arch-service serve
./run.sh                       # the same: build, then serve on 3022 (./run.sh --no-build: no build)
```

Build against SimOxide's `main` (`../simoxide`, path dependencies; at least `1429209`, which has
the path monitoring and `fix_double_resume`) and `../palladio-sim-service`. The binary needs no
files at run time.

| Variable | Default | Meaning |
| --- | --- | --- |
| `PORT`, `HOST` | `3022`, `0.0.0.0` | where it listens |
| `BACKEND_API_URL` | `http://localhost:8080/api` | the mdeo backend whose session tokens it verifies (JWKS) |
| `JWT_ISSUER` | `mdeo-platform` | expected token issuer |
| `SIM_THREADS` | one per CPU | simulations at once (each one thread) |
| `SIM_RESULT_CACHE_SIZE` | `4096` | evaluations kept (LRU) |
| `PCM_CACHE_MB` | `1024` | memory bound of the kept evaluations (estimated), LRU |
| `SIM_TIMEOUT_SECONDS` | `600` | hard wall-clock limit per simulation (the call's `wallSeconds`, default 60, is usually lower) |
| `SIM_MAX_EVENTS` | `0` (none) | hard event limit per simulation |
| `MAX_SESSIONS` | `256` | concurrent script sessions |
| `SIM_DUMP_DIR` | unset | writes every received model there (`received-*.txt`), for debugging |
| `PCM_DETAIL_ENDPOINT` | unset | `1` enables `POST /pcm/detail` (unauthenticated simulations; leave off on a shared server) |

**Registering it in mdeo:** under **Settings → Plugins** add the URL the service is reachable at
(e.g. `http://host:3022/`); mdeo reads the manifest from `GET /` and opens script sessions on
`/ws/sessions/...`. Enable **Palladio Architecture** in a project; scripts then see
`pcmEvaluate` and its records (`PcmEvaluation`, …).

**Version:** the manifest's `version` (also in its descriptions) and every `PcmEvaluation`'s
`version` field are `<date of the last commit>+<git hash>` (`-dirty` for a build with uncommitted
changes), set by `build.rs`. `curl -s http://host:3022/ | jq .version` shows what is deployed.

## The function

`pcmEvaluate(end, warmUp = 0, binSeconds = 10, msPerTimeUnit = 1000, profile = null, groups =
null, services = null, overrides = null, percentiles = null, detailCalls = false,
histogramPerDecade = 10, seed = 1, usageStreams = true, fixDoubleResume = true, maxPods = 400,
hpaSyncSeconds = 15, hpaTolerance = 0.1, hpaDownscaleStabilisationSeconds = 300,
hpaCpuInitialisationSeconds = 300, maxPathSets = 10000, wallSeconds = 60, maxEvents = 0)` →
`PcmEvaluation`. The contract, with every record field, is
[`API-pcmEvaluate.md`](API-pcmEvaluate.md). In short (`src/evaluate.rs`, `src/dynamic.rs`,
`src/lb.rs`):

- *Model.* The PCM with processing resources overridden (`PcmOverride`). Every
  `PcmScalingGroup` (a pod container; min/max/initial pods, target utilisation, start-up and
  scale-down seconds, CPU request, an optional Kubernetes `behavior` as JSON) is an SPD elastic
  infrastructure with a `HorizontalPodAutoscaler` (SimOxide `docs/spec/hpa.md`, Slingshot's
  adjustments); its assemblies are SPD service groups behind load balancers (`PcmServiceGroup`,
  or the plugin's: Slingshot's pattern, a branch with one call per pod, on a container
  `LoadBalancerServer` without work). The model's monitors are replaced by Response Time of
  every usage scenario, entry-level system call, system operation and, with `detailCalls`,
  assembly operation.
- *Run.* SimOxide, fast mode, SimuLizar flavour, virtual-time processor sharing, the request's
  seed, per-scenario load profile (`PcmLoadStep`, LP-1) and usage streams (RND-11), SimuLizar's
  double resume fixed by default (REF-2), SimOxide's path monitoring (MEAS-13: synchronous paths
  per session, call counts) and busy-time hooks read at the window's start, at every bin edge and
  at the end (pauses do not change the run). The wall-clock (`wallSeconds`) and event limits
  bound one simulation.
- *Result* over the window `[warmUp, end]` and per bin: response times per system operation and
  usage scenario (and per entry-level call and assembly operation with `detailCalls`): count,
  mean, max, the requested percentiles, a log histogram, per-bin counts, means and percentiles;
  per processing resource of every container: utilisation (window, per bin, max bin), ready and
  all pods (mean, peak, per bin), replica-core-seconds, the group's HPA activity; call counts per
  ExternalCallAction and EntryLevelSystemCall; per usage scenario the distinct container sets of
  the sessions' synchronous paths with their counts (pods count as their scaled container);
  created pods, rescales, peak pods, reversals.
- *Failures.* A simulation that fails, hits the pod cap, the event limit or the wall-clock
  limit returns `failed = true` with a `message` (and `capped` / `timedOut`), never an error of
  the call; a panic is caught the same way. Wrong arguments (a group on something that is not a
  ResourceContainer, a percentile outside 0..100) are errors of the call.
- *Invariance.* Results do not depend on element names, ids or list order: the PCM is
  brought into a canonical form first (`src/canon.rs`, colour refinement of the instance graph;
  92 ms for a realised MiSArch design of 8 065 PCM instances, once per model), simulated, and the
  results are mapped back to the caller's instances, lists in canonical order. Residual:
  interchangeable instances (structurally identical, e.g. unused identical containers) are
  ordered by name; their values are equal as a multiset, which caller instance gets which may
  follow the names.
- *Caching.* Deterministic, so cached per (digest of the model's canonical PCM, arguments) in the
  session's model and in the process (bounded by `SIM_RESULT_CACHE_SIZE` and `PCM_CACHE_MB`); a
  script can call `pcmEvaluate` with the same arguments in every metric function for one
  simulation. Timed-out runs are not cached.

**Design detail** (`src/detail.rs`): `palladio-arch-service detail MODEL [REQUEST.json] [--out
FILE]` (MODEL `.m` or mdeo model data `.json`), or `POST /pcm/detail` with the request plus
`modelText` or `model` (only with `PCM_DETAIL_ENDPOINT=1`). The request is `pcmEvaluate`'s in
JSON (API doc) plus `budgetsMs`, `sloFactor`, `histogramPerDecade`, `assemblyOperations`. It runs
**the same simulation** with more monitors (assembly operations, every core of every processing
resource; monitors do not change the run) and returns response-time statistics, histograms and
per-bin series per scenario, call, system and assembly operation, utilisation, queue length and
wait per resource, utilisation and replicas per container over time, SLO violations over time
for the budgeted series, the load profile with the measured session rate per unit of the
profile's factor, and the run's evaluation under `evaluation`. `palladio-arch-service evaluate
MODEL REQUEST.json` prints only that evaluation.

## Command line

```text
palladio-arch-service serve                      the plugin service (the default)
palladio-arch-service evaluate MODEL [REQUEST.json] [--out FILE]
                                                 pcmEvaluate on a model, the PcmEvaluation as JSON
palladio-arch-service detail MODEL [REQUEST.json] [--out FILE]
                                                 the design detail of the same run (above)
```

## Verification

`cargo test --release` (`tests/evaluate.rs`, 10 tests; unit tests for the manifest with its
defaults, the canonical form, the detail request and step means). Fixtures: `tests/fixtures/two-tier.m` (Frontend on
C1, 5 ms, calls Backend on C2, 10 ms) and `tests/fixtures/paths.m` (Frontend calls Backend with
probability 0.3 and forks Audit on C3 asynchronously):

| Test | Check |
| --- | --- |
| `counts_response_times_and_core_seconds` | session, operation and call counts against the arrival rate; requested percentiles, histogram and per-bin series consistent with the window's; utilisation λD per container and per bin; cores × window as core-seconds; deterministic; `detailCalls` adds calls and assembly operations without changing the run |
| `sync_path_sets` | the path sets {C1} and {C1, C2}, the latter 30 % (± 4 σ), C3 (asynchronous) never; the cap keeps the most frequent; call counts 1 and 0.3 per session |
| `autoscaled_replicas_cost_and_paths` | the HPA settles at two pods at one core of load; core-seconds, per-bin pods, utilisation per pod; every session's set is {C1, C2} whichever pod served it; the plugin's load balancers are not reported |
| `hpa_settings_are_arguments` | a `behavior` limiting scale-ups and a shorter sync period change the run |
| `failures_are_evaluations` | pod cap, wall-clock limit, event limit and an unallocated assembly give `failed` (`capped`, `timedOut`); wrong arguments are errors |
| `load_profile_and_usage_streams` | a per-scenario step raises the sessions after it; another design sees the same users |
| `the_detail_is_the_evaluated_run` | the detail's `evaluation` equals `pcmEvaluate`'s; the JSON request maps to the same request |
| `the_script_function` | `pcmEvaluate` as the platform calls it (records with defaults, nested lists); cached |
| `results_do_not_depend_on_names_or_order` | the fixtures (also autoscaled, with a profile and an override) with every id and entity name changed and every list shuffled give the same evaluation, element for element |
| `a_realised_misarch_design_does_not_depend_on_names_or_order` | the same on a realised design of the domain project (8 065 instances; interchangeable instances compared by value) |

Measured (release, this dev VM, one simulation thread), the today design of the domain project
(`build/domain/detail-work/domain-w-today-*/realised/design.model.json`, 34 autoscaled services,
105 containers): the compressed day (C4, 1320 s incl. 120 s warm-up, 10 s bins) 4.4 s wall, 16.8 M
events, 0.32 GB RSS, 10 pods created, 1 823 path sets, 1.9 MB of JSON; 120 s of steady load 0.6 s,
1.4 M events, 0.10 GB. The reproduction of SimuLizar's double resume (3600 s, seed 99) completes
in 6.1 s (0.21 GB); with `fixDoubleResume = false` it returns `failed` with SimuLizar's error.

## Layout

| Path | Contents |
| --- | --- |
| `src/functions.rs` | The manifest (`pcmEvaluate`, its records and defaults) and the call |
| `src/evaluate.rs` | The request, the run's reduction to the evaluation, the bounded cache |
| `src/dynamic.rs` | The run: SPD and HPA configuration, SimOxide with path monitoring, readings at the bin edges |
| `src/lb.rs` | The load balancers of SPD's service groups (Slingshot's pattern) |
| `src/detail.rs` | The design detail and the monitors |
| `src/pcm.rs` | The plain PCM of a model in canonical form, held per model, with overrides |
| `src/canon.rs` | The canonical form (names and order from the structure) and the way back |
| `src/graph.rs` | The model as an editable instance graph (keeps opposite ends) |
| `src/mfile.rs` | `.m` and model data reader |
| `src/session.rs`, `src/server.rs` | Sessions and HTTP/WebSocket, adapted from palladio-sim-service |
| `build.rs` | The version string |
| `tests/` | The tests and their fixtures |

## Limitations

- Utilisation of an autoscaled container is per ready pod; a pod's CPU is tracked from the
  first bin edge after its creation (a scale-out mid-bin loses at most that bin's part).
- Call counts are per external call action, summed over all assembly contexts of its component.
- Load balancers serve components with one provided interface and callers that read no return
  value (Slingshot's load balancers hand nothing back).
- Times inside a group's `behaviorJson` are model time units (seconds when msPerTimeUnit = 1000).
