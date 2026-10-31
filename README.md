# Palladio Simulation plugin service (`palladio-sim-service`)

An mdeo plugin service, written in Rust, that does one thing: it **simulates the plain Palladio
(PCM) part of a model with [SimOxide](../simoxide)** and returns what the simulation measured.
One script function, `pcmEvaluate`, runs one dynamic simulation (per-scenario load profile,
Kubernetes-style autoscaling through SPD and SimOxide's HPA) and returns raw measurements:
response-time statistics, utilisation and pods per processing resource, call counts and the
container sets of the sessions' synchronous paths, over a window and per time bin. Every
interpretation (cost, failure model, budgets, consistency, which percentile matters) belongs to
the mdeo scripts. It also owns the Palladio metamodel as mdeo sees it (`gen-metamodel`) and the
PCM ⇄ mdeo conversion (`import`, the XMI writer). The test fixtures use their own copy of that
metamodel (`tests/fixtures/metamodel/`, written by `gen-metamodel`; a test keeps it current).

**One repository, one plugin (2026-10-22).** This repository merges the former
`palladio-sim-service` (the Palladio Simulation plugin: metamodel, import, protocol, auth,
`pcmSimulate`/`pcmStat`; tag `sim-service-final` = its last commit a5ee9d6) and the former
`palladio-arch-service` (the architecture plugin: `pcmEvaluate`, the design detail; tag
`arch-service-final`). Both histories are reachable (`git log --all`). Dropped in the merge:
`pcmSimulate`, `pcmStat` and their engine (compiled-model cache, replications), the second
session/server layer, the `summary` command. `pcmEvaluate` replaces them.

The analytic functions (`pcmResources`, `pcmScenarios`, `pcmOperations`, `pcmCallRates`,
`pcmWeightedVisits`, `pcmOperationVisits`, `pcmFailure`, `pcmSourceFailure`,
`pcmOperationFailure`, `pcmWarnings`), the StoEx evaluator `pcmMean`, the per-call simulation
functions (`pcmSimulate`, `pcmStat`, `pcmRun`, `pcmRunStat`, `pcmHpaRun`) and the layer-2
translation with its `oracle` and `translate` commands were removed on 2026-10-22; tag
`pre-simulation` has them (tag `pre-cleanup` the earlier `arch*` functions).

## Deployment

```bash
cargo build --release          # → target/release/palladio-sim-service
PORT=3022 target/release/palladio-sim-service serve
./run.sh                       # the same: build, then serve on 3022 (./run.sh --no-build: no build)
```

Build against SimOxide's `main` (`../simoxide`, path dependencies; at least `1429209`, which has
the path monitoring and `fix_double_resume`). The binary needs no files at run time. The cargo
feature `exact` (SimuLizar-exact mode, only for the `roundtrip` command's comparison with the
reference) links code without a licence grant (SimOxide's LICENSES, section 7): never deploy a
build with it.

**Container image.** `Dockerfile` builds the same binary in Docker: it fetches SimOxide at the
commit in `simoxide.rev` (the commit this repository builds against; update it together with a
SimOxide change), refuses the feature `exact`, and runs on Debian slim (glibc malloc, not musl)
as a non-root user, port 3022, with a health check on `GET /`. `.github/workflows/docker-publish.yml`
publishes `ghcr.io/<owner>/palladio-sim-service:<version>` on a tag `v<version>` (and
`sha-<commit>` on a manual run).

```bash
docker build --build-arg BUILD_JOBS=4 -t palladio-sim-service .
docker run --rm -p 3022:3022 -e BACKEND_API_URL=http://backend:8080/api -e SIM_THREADS=16 palladio-sim-service
```

`--build-arg CARGO_FEATURES=mimalloc` builds with mimalloc instead of glibc's allocator (not
measured against it). In mdeo's single-server deployment the instances run behind one load
balancer and are sized from its manager (mdeo-cloud `infra/manager/`).

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
| `SIM_DUMP_DIR` | unset | writes every received model there as the platform sent it (`received-<pid>-<n>.json`, a wire model keeping every value's type) and every `pcmEvaluate` request beside it (`….request-<k>.json`); `palladio-sim-service evaluate received-….json ….request-<k>.json` re-runs exactly that evaluation |
| `RUST_LOG` | `info` | log filter (tracing) |
| `PCM_DETAIL_ENDPOINT` | unset | `1` enables `POST /pcm/detail` (unauthenticated simulations; leave off on a shared server) |

**Registering it in mdeo:** under **Settings → Plugins** add the URL the service is reachable at
(e.g. `http://host:3022/`); mdeo reads the manifest from `GET /` and opens script sessions on
`/ws/sessions/contrib/palladio-arch/functions`. Enable **Palladio Simulation** in a project;
scripts then see `pcmEvaluate` and its records (`PcmEvaluation`, …). The manifest keeps the ids
of the former architecture plugin (plugin `palladio-arch-service`, contribution `palladio-arch`),
so projects and compiled scripts that use `pcmEvaluate` need no change; a registration of the
former Palladio Simulation plugin (`palladio-sim-service`, contribution `palladio`, port 3020)
is obsolete and should be removed (its `pcmSimulate`/`pcmStat` no longer exist).

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
  follow the names. Nor does it depend on how values were sent: the platform sends integers as
  `Int`, enums as `Enum` and unset attributes as empty lists, model data on disk has doubles,
  strings and nulls; the canonical form normalises them, so the script path (`pcmEvaluate`) and
  the command line (`evaluate`, `detail` on the model-data file) give the same evaluation
  (`the_script_path_and_the_command_line_give_the_same_evaluation`, on a realised MiSArch design
  as dumped from a search).
- *Caching.* Deterministic, so cached per (digest of the model's canonical PCM, arguments) in the
  session's model and in the process (bounded by `SIM_RESULT_CACHE_SIZE` and `PCM_CACHE_MB`); a
  script can call `pcmEvaluate` with the same arguments in every metric function for one
  simulation. Timed-out runs are not cached.

**Design detail** (`src/detail.rs`): `palladio-sim-service detail MODEL [REQUEST.json] [--out
FILE]` (MODEL `.m` or mdeo model data `.json`), or `POST /pcm/detail` with the request plus
`modelText` or `model` (only with `PCM_DETAIL_ENDPOINT=1`). The request is `pcmEvaluate`'s in
JSON (API doc) plus `budgetsMs`, `sloFactor`, `histogramPerDecade`, `assemblyOperations`. It runs
**the same simulation** with more monitors (assembly operations, every core of every processing
resource; monitors do not change the run) and returns response-time statistics, histograms and
per-bin series per scenario, call, system and assembly operation, utilisation, queue length and
wait per resource, utilisation and replicas per container over time, SLO violations over time
for the budgeted series, the load profile with the measured session rate per unit of the
profile's factor, and the run's evaluation under `evaluation`. `palladio-sim-service evaluate
MODEL REQUEST.json` prints only that evaluation.

## Command line

```text
palladio-sim-service serve                       the plugin service (the default)
palladio-sim-service evaluate MODEL [REQUEST.json] [--out FILE]
                                                 pcmEvaluate on a model, the PcmEvaluation as JSON
palladio-sim-service detail MODEL [REQUEST.json] [--out FILE]
                                                 the design detail of the same run (above)
palladio-sim-service gen-metamodel [DIR]         palladio.mm and pcm/repository.mm (below)
palladio-sim-service import [--readable] DIR [MM]
                                                 the PCM XMI model of DIR as an mdeo model (.m)
palladio-sim-service roundtrip DIR...            PCM -> mdeo -> PCM for corpus models, simulated and compared
```

## The Palladio metamodel

`palladio-sim-service gen-metamodel [DIR]` writes the metamodel into `DIR`. It is generated from
SimOxide's PCM 5.2 Ecore tables and holds the classes and features the simulator reads, 155
classes. Models and scripts use `palladio.mm`.

| File | Contents |
| --- | --- |
| `pcm/repository.mm` | The base part, which imports nothing: component types, interfaces, signatures, roles, data types (33 classes) |
| `palladio.mm` | Imports the base part and holds everything else (122 classes); the mapping rules; one view per package group |

A class lives in the base part when its PCM package is `pcm.core` or `pcm.repository`, unless the
metamodel language forces it into `palladio.mm`:
- A file may name an association end only on a class of its own. A containment or pair of
  opposites is named at both ends, so its two classes share a file.
- Parameter usages (`VariableUsage`) are contained by component implementations, SEFF actions,
  assembly contexts and usage model calls, so these share `palladio.mm`.
- Only `BasicComponent`, `CompositeComponent`, `ImplementationComponentType` and `PassiveResource`
  move.
- Each enum lives in the first file that uses it.

Where a class lives does not decide how the metamodel is shown: `palladio.mm` declares a view for
every package group (`Core`, `Repository`, `Behaviour`, `Parameters`, `Composition`,
`ResourceEnvironment`, `Allocation`, `UsageModel`, `Monitoring`), listing the group's classes from
either file. The metamodel editor switches between them in its toolbox.

`cargo test` checks these rules on the generated layout (`mm::tests`).

Mapping rules:
- Classes keep their PCM names.
- Features drop their EMF suffix (`components__Repository` → `components`).
- An instance's name is its PCM id.
- Stochastic expressions are strings holding the StoEx (`processingRate = "2.5"`).
- Variable references are dot-separated names (`"items.NUMBER_OF_ELEMENTS"`).
- References into Palladio's library models are enums: `ProcessingResourceType.CPU`,
  `SchedulingPolicy.PROCESSOR_SHARING`, `MetricDescription.RESPONSE_TIME`, ...
- Containment is composition.
- All multiplicities are optional (`[?]`, `[*]`), so any PCM file converts.
- PCM lists are ordered, but mdeo association ends are not: the platform sends them sorted by
  name. The service therefore orders every list by the declaration order of the instances.
  Imported models are written in PCM tree order, so they get back exactly their original order.

The same mapping serves three purposes:
- writing the metamodel;
- importing PCM models: `palladio-sim-service import [--readable] DIR [../palladio.mm] > model.m`;
- turning received models back into PCM XMI for the simulator, one file per root instance,
  written as EMF writes them.

The domain project's `metamodel/palladio.mm` and `metamodel/pcm/repository.mm` are this
command's output byte for byte (checked 2026-10-22).

## Parallelism

- **Simulations** run on `SIM_THREADS` blocking threads, never on the async runtime. Calls of
  all sessions spread over the same threads; equal requests wait for one simulation.
- **Sessions** are independent; each handles its calls in order, as the protocol requires.
- **Keepalive.** Every session pings every 30 s and drops a peer silent for 90 s, and keeps
  answering while a long simulation runs.

Throughput through the protocol (`BENCH_SESSIONS` sessions × `BENCH_CALLS` calls of the
SimuLizar mediastore example, 600 s each, a seed per call):

```bash
SIM_THREADS=22 cargo test --release --test session throughput -- --ignored --nocapture
```

## Verification

`cargo test --release`: `tests/evaluate.rs` (10 tests), `tests/session.rs` (4 tests and the
ignored throughput benchmark: the service end to end over its WebSocket endpoint with
self-signed RS256 tokens, corpus models imported and evaluated through the protocol equal to the
library's evaluation, refusal codes 4401/4404/4409, `unknown-model`, 24 parallel sessions),
`tests/kotlin_compat.rs` (ignored; against the platform's kotlinx-serialization classes), unit
tests (manifest with its defaults, canonical form, protocol decoding of the specification's
reference bytes, metamodel layout rules, schema, detail request, step means).
`palladio-sim-service roundtrip` on SimOxide's corpus (`h*`, `x_sl*`): PCM → mdeo → PCM
simulates identically; in a local `--features exact` build also byte-identical to SimuLizar's
stored `measurements.csv` (39/39 checked 2026-10-22). Fixtures: `tests/fixtures/two-tier.m` (Frontend on
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
| `src/schema.rs` | The Palladio metamodel as mdeo sees it, derived from the PCM Ecore tables |
| `src/mm.rs`, `src/mtext.rs` | Writers for `.mm` (split into parts) and `.m` |
| `src/import.rs` | PCM XMI → mdeo model |
| `src/xmi.rs` | mdeo model → PCM XMI |
| `src/protocol.rs`, `src/session.rs`, `src/server.rs`, `src/auth.rs`, `src/config.rs` | `script-functions` protocol (CBOR), sessions, HTTP/WebSocket, token verification, configuration |
| `examples/scc.rs` | A helper that prints the metamodel's dependency groups |
| `build.rs` | The version string |
| `tests/` | The tests and their fixtures |

## Limitations

- Utilisation of an autoscaled container is per ready pod; a pod's CPU is tracked from the
  first bin edge after its creation (a scale-out mid-bin loses at most that bin's part).
- Call counts are per external call action, summed over all assembly contexts of its component.
- Load balancers serve components with one provided interface and callers that read no return
  value (Slingshot's load balancers hand nothing back).
- Times inside a group's `behaviorJson` are model time units (seconds when msPerTimeUnit = 1000).
