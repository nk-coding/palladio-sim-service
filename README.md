# Palladio Architecture plugin service (`palladio-arch-service`)

An mdeo plugin service, written in Rust. It evaluates Palladio (PCM) models that carry
architecture decisions (layer 2, `architecture.mm` of
[palladio-architecture-optimization](../palladio-architecture-optimization)). It knows PCM and
layer 2 only: a domain layer such as the microservice layer (`microservices.mm`) is realised in
the model itself, by that layer's model transformations, with PCM and the generic completions of
layer 2 (`Colocation`, `Work`, `Redirect`, `Notification`, `Replica`; the HPA project's
DESIGN.md H10-H16). For the model a
script runs on, it translates the decisions into plain PCM and simulates the result with
[SimOxide](../simoxide), through [palladio-sim-service](../palladio-sim-service), which it uses as
a library (protocol, auth, PCM schema, XMI writer, simulation engine). The design is
`palladio-architecture-optimization/DESIGN.md` §4–§7; its §12 lists the decisions taken here.

## Run it

```bash
./run.sh              # build (release), then serve on http://localhost:3022
./run.sh --no-build   # start the last build
```

Register `http://localhost:3022/` under **Settings → Plugins** and enable **Palladio
Architecture** in a project. In the dev stack the service runs in the `palladio-arch` window of
the `mdeo-dev` tmux session and logs to `service.log`. The environment variables are those of
palladio-sim-service (`SIM_THREADS`, `SIM_DUMP_DIR`, ...), except that the default port is 3022.
Cap the caches for large models: a compiled MiSArch model (9 200 PCM objects after translation)
takes about 10 MB, so the default `SIM_MODEL_CACHE_SIZE=256` can hold about 2.5 GB; the dev stack
runs with `SIM_MODEL_CACHE_SIZE=16 SIM_RESULT_CACHE_SIZE=1024`. `ARCH_TIMING=1 … eval MODEL` prints
the time of each step (MiSArch, P6 models, measured 2026-10-16: read 10 ms, translation 80–95 ms,
compilation 130–170 ms, simulation of 22 000 measurements 190–240 ms).
`SIM_DUMP_DIR` also receives every model as the platform sent it (`received-*.txt`).

## Functions

| Function | Returns |
| --- | --- |
| `archSimulate(seed = 1, maxMeasurements = 10000, maxSimTime = -1, replications = 1, simulateLinkingResources = false)` | `ArchResult`: what the monitors measured on the translated model (fields as palladio-sim-service's `SimResult`) |
| `archStat(element, metric = "Response Time", statistic = "mean", ...)` | one number, as `pcmStat` |
| `archCost()` | cost from the `CostModel` (DESIGN §12 D13; `ContainerPrice`s, D22) |
| `archModularity()` | cohesive MQ of the call graph between the movable components, containers as clusters (D32) |
| `archUtilisation(element = "")` | offered load: of a container (its highest resource), of `node:<name>`, or the highest of all |
| `archReplicas(element = "")` | the replicas of a container with a `Deployable`, fixed or autoscaled; "" = the sum over those in use (D50) |
| `archFailure(exclude = "")` | O2: the rate-weighted share of requests that fail (D53): per request, the probability that it needs each container on its synchronous path, times the container's unavailability (shared fate; `Reliability`, `ChangeUnit`) |
| `archChangeCoupling()` | O3: containers touched per change of a `ChangeUnit`, weighted by its changes: its own deployables plus, per other deployable that calls into it, the largest break probability of those calls (D54) |
| `archInconsistency(exclude = "")` | O4: per request, stale reads (cache hits; across containers, the share `staleReads` of a call's mechanism; calls into a `Replica` apart from its source) plus writes to other containers (`CallSite.writes`; weights in `Consistency`) (D54) |
| `archLatencyViolation()` | C1: Σ over the `LatencyBudget`s of max(0, no-load time / budget − 1) (D55) |
| `archPeakUtilisation()`, `archPeakViolation()` | C2: the highest utilisation of the `PeakCapacity` containers at its factor, and its excess over the limit (D55) |
| `archNoLoadTime(exclude = "")` | the rate-weighted mean response time without contention over the usage scenarios (but those named in `exclude`, comma-separated), in model time units: the expected-value walk of the translated model, no simulation (DESIGN D48; static objectives) |
| `archInfo()` | `ArchInfo`: translated or not, cost, modularity, every resource's load, warnings |
| `archHpaSimulate(seed = 1, exclude = "login")` | `ArchHpaResult`: a dynamic run with Kubernetes-style autoscaling over the model's `LoadProfile` (rule 5, `src/dynamic.rs`, below): cost, SLO violations, lag, flapping, pods, per autoscaler |
| `archHpaCost(seed = 1, exclude = "login")` | its cost per time over the profile's window: the fixed cost plus each autoscaled container's price per pod times its ready pods (the dynamic O1) |
| `archHpaSlo(seed = 1, exclude = "login")` | its share of requests (all scenarios with a `LatencyBudget` but `exclude`) slower than `sloFactor` × their budget |
| `archHpaStat(statistic, seed = 1, exclude = "login")` | one measure of the run: `cost`, `idealCost`, `readyReplicaSeconds`, `slo`, `sloSteps`, `requests`, `meanMs`, `p95Ms`, `lag`, `lagMax`, `flapping`, `peakReplicas`, `rescales`, `createdPods`, `capped` |

A model without decisions (or with decisions that change nothing) is simulated exactly as
palladio-sim-service simulates it. Simulations are shared and cached as there.

**Dynamic runs** (`archHpa*`; the HPA project's DESIGN.md H2–H4). Rule 5 (`translate_dynamic`)
turns the translated model into the structure SimOxide's HPA scales (SimOxide `DESIGN-SPD.md`
§5.10): every autoscaled `Deployable` becomes one pod (⌈q⌉ cores at the per-core speed of rules 3
and 4, an HPA request of q full-speed cores), each assembly on it gets a load balancer (Slingshot's
pattern: one branch with one call per pod, on `LoadBalancerServer`), its sidecar the cores of
`maxReplicas` pods; an SPD elastic infrastructure per pod, a service group per assembly, and one
`HorizontalPodAutoscaler` per Deployable (`averageUtilization` = 100 · `targetUtilisation`,
`minReplicas`, `maxReplicas`, `startupSeconds`, `scaleDownSeconds`; absent: 1, twice the ideal count
at the profile's peak (at least 4), 30 s, Kubernetes' 5 min). The run starts each group at the
ideal autoscaler's count of the profile's first factor, applies the `LoadProfile` as SimOxide's
load profile (every usage scenario) and measures over `[warmUpSeconds, endSeconds]`. Lag: per load
increase and autoscaler that needs pods, the time until its ready pods reach the ideal count / 1.1;
flapping: direction reversals of rescales per hour. A run that creates more than `HPA_MAX_PODS`
pods (default 400) stops and is penalised (capped: every pod at its maximum, every request over
budget). Outcomes are cached by content (`SIM_RESULT_CACHE_SIZE`); at most `SIM_THREADS` run at
once. `HPA_TRACE_DIR` writes a run's response times and pods over time.

## Generic Palladio functions (`pcm*`, branch `pcm-only`)

Plain PCM in, numbers out: they read the model's PCM part as it is (instances of other classes
are ignored) and know nothing above PCM; whatever says how to read the model (overrides, branch
probabilities, unavailabilities, transparent assemblies, load profiles, scaling groups, budgets)
comes from the script. Used by the domain project (`palladio-architecture-optimization-domain`,
DESIGN.md §B and §H), whose layer 2 is realised in the model by transformations. Times in model
time units unless a name says `Ms`/`Seconds`.

| Function | Returns |
| --- | --- |
| `pcmResources()` | `PcmResource` per processing resource: container, kind, rate, replicas, demand, busy, utilisation |
| `pcmScenarios()` | `PcmScenario` per usage scenario: arrival rate, no-load response time |
| `pcmOperations()` | `PcmOperation` per entry-level system call: signature, callee, calls per time unit, no-load time per call |
| `pcmWeightedVisits(actions, weights, exclude = "")` | rate-weighted mean over the scenarios of Σ expected executions per request × weight |
| `pcmOperationVisits(actions, weights)` | `PcmOperationValue` per entry-level call: Σ expected executions per call × weight (the call itself counts once) |
| `pcmCallRates(transitions, probabilities)` | `PcmCall` per external and entry-level call: calls per time unit, branch probabilities overridden |
| `pcmFailure(containers, unavailabilities, transparent, exclude = "")` | rate-weighted share of requests needing an unavailable container on their synchronous path |
| `pcmOperationFailure(containers, unavailabilities, transparent)` | `PcmOperationValue` per entry-level call: the probability that one call fails so |
| `pcmSimulate(overrides, ...)`, `pcmStat(overrides, element, metric, statistic, ...)` | a simulation with processing resources' replicas and rates overridden (`PcmOverride`) |
| `pcmRun(overrides, profile, usageStreams = false, ...)`, `pcmRunStat(...)` | the same with a per-scenario load profile (`PcmLoadStep` {scenario or null = every scenario, time, factor}; SimOxide LP-1) and SimOxide's usage streams (common random numbers, RND-11) |
| `pcmHpaRun(overrides, groups, services, profile, budgetElements, budgetsMs, warmUp, end, msPerTimeUnit, sloFactor, usageStreams = false, seed = 1, maxPods = 400)` | `PcmHpaResult`: a dynamic run with SPD and SimOxide's HPA: per `PcmScalingGroup` (container = the pod, min/max/initial replicas, target utilisation, start-up, scale-down window (< 0: default), CPU request (≤ 0: the pod's cores)) an elastic infrastructure, per `PcmServiceGroup` (assembly, its load balancer) a service group (a group without any gets load balancers from the plugin, rule 5's pattern); replica-seconds, peak, rescales, created pods, flapping, the share over `sloFactor` × budget of the budgeted elements (scenarios, calls, or signatures as system operations), mean and p95 of the calls; cached per model and request |
| `pcmMean(expr)`, `pcmWarnings()` | the mean of a variable-free StoEx; the analysis' warnings |

The analysis behind them walks expectations: a discrete probability function literal as an input
of an entry-level call (`IntPMF`, `EnumPMF`, ...) is enumerated (one walk per value combination,
exact), and only expressions that draw random numbers make a walk fall back to 4 000 sampled
walks.

**Design detail** (`src/detail.rs`): `palladio-arch-service detail MODEL [REQUEST.json] [--out
FILE]` (MODEL `.m` or mdeo model data `.json`), or `POST /pcm/detail` with the request plus
`modelText` or `model` when the service runs with `PCM_DETAIL_ENDPOINT=1`. One simulation with full
monitoring (scenarios, entry-level calls, system operations, assembly operations except recursive
ones, every core of every processing resource), reduced to JSON named by PCM elements:
response-time statistics, log histogram and per-bin mean/p95/throughput per series, utilisation,
queue length and wait per resource, utilisation and replicas per container over time, SLO
violations over time for the budgeted series, the load profile. Request: `durationS`, `warmUpS`,
`binS`, `timeUnitMs`, `seed`, `usageStreams` (default on), `profile` [{scenario?, timeS,
factor}], `overrides` [{resource, replicas, rate}], `budgetsMs` {key or name: ms}, `sloFactor`,
`histogramPerDecade`, `assemblyOperations`, and `autoscaling` {groups, services, maxPods} for a
dynamic run.

## Command line

```text
palladio-arch-service translate MODEL OUT_DIR    OUT_DIR/<name>.translated.m (plain PCM, formatted with the
                                                 project's tools/format_model.py) and OUT_DIR/pcm/ (PCM XMI)
palladio-arch-service eval MODEL [--seed N] [--measurements N] [--max-sim-time S] [--replications N]
                                 [--plain] [--json]
                                                 translate, simulate, print series, loads, cost, modularity
palladio-arch-service hpa MODEL [--seed N] [--exclude A,B] [--json] [--dump DIR]
                                                 a dynamic run over the model's load profile; --dump writes its
                                                 PCM, SPD and HPA files
palladio-arch-service serve                      the plugin service (the default)
```

`MODEL` is an mdeo `.m` file. `--plain` drops the decisions; `--json` prints machine-readable
output for comparisons (e.g. with `msa-sim eval`).

## Translation

Applied to a copy of the model, in this order (`src/translate.rs`):

| Step | Decision | PCM result |
| --- | --- | --- |
| 0 | `Colocation` | the assembly (which has no AllocationContext) is allocated in the container of its host, after the walks of the model before translation (placement, modularity, O3, O4 do not see it) |
| 0 | `Work` | the InternalAction gets exponential demands, `cpuMs` on the CPU, then `hddMs` on the HDD, at the rates of the container of its component's first assembly (none without an assembly) |
| 0 | `Workflow` with an `Orchestrated` style | every step (the call with its loop or branch) leaves its SEFF; the trigger's component makes them at the end of the trigger's SEFF, `stepCpuMs` each, as a forest (parallel siblings) or level by level; an asynchronous step to another container is a request and a reply through the broker; all in the background when the first step is asynchronous and remote (D42) |
| 0 | `CallSite` with a `Bypass` | across containers: the callee's top-level CPU work on the caller, `latencyMs`, and the callee's calls into `direct` containers, made by the caller (D40) |
| 0 | `CallSite` with a `ComposedCall` | across containers: the call (with its loop or branch) moves after the composer's call into the caller: `composeCpuMs`, then the composer's cache branch or the call; several in parallel (D39) |
| 0 | `Redirect` | the call is re-pointed to `service` of `provider` through a role of the caller's component (wired from all its assemblies) and loses its parameter and return usages; its callee does not run, also not in the walks before translation |
| 0 | `Notification` | right after `after`: `cpuMs` on its container's CPU, then an asynchronous fork with one call per `Delivery`; `published`, with a broker: first one publish through the publisher of the container (on its sidecar), the deliveries are messages whose stubs only travel (D43). Several after one action: the published ones end up last |
| 0 | a `Mechanism` of another class | the call as modelled (that layer realises it with the completions); its `couplingWeight` and `staleReads` feed the modularity and O4 |
| 1 | `AsyncCall` | the call inside a `ForkAction` with an asynchronous forked behaviour; through the broker to another container, no fork here: the stub publishes on the caller's path and forks the rest (D43) |
| 1 | `CachedCall` | a probabilistic branch: hit = CPU `hitCpuMs` on the caller, miss = the call |
| 2 | `RemoteCalls` | per assembly connector between two containers: a stub with the caller and a skeleton with the callee; one SEFF per signature, forwarding parameters and return values. Plain call: caller CPU, latency, call / callee CPU, call. Asynchronous: publish, then broker, latency, call in the background / deliver, call. Delivery of a published Notification: broker, latency, call / deliver, call. Orchestrated request: publish, broker, latency, call, broker, latency, deliver / deliver, call, publish (D43). Connectors into a `direct` container are left alone (D19); a sidecar has `sidecarCores` per replica (D20) |
| 3a | `Deployable.autoscaling` → `Autoscaling` | n = the smallest count ≥ 1 with u/(n·q) ≤ `targetUtilisation` and u_s/(n·sidecarCores) ≤ target, u and u_s the CPU loads of the container and its sidecar (full-speed cores, one walk after rule 2), capped at `maxReplicas`; the sidecar gets n·sidecarCores cores; per `LoadLevel` the same with the load × factor, for the cost's weighted mean (D50) |
| 3 | `Deployable` | n replicas of q cores: `numberOfReplicas` = n·⌈q⌉, `processingRate` × q/⌈q⌉ (limit-speed); with `quota`, full speed here |
| 4 | `Deployable.quota`, `Node` | one load analysis (its walk gives the no-load times); a quota container runs at max(q′, 1 − u/(n⌈q⌉)·(1 − q′)/q′) of full speed (D45); node slowdown s = 1 + C(c, U)/(c − U) from the node's CPU load U; every CPU on the node runs at most at 1/s of full speed |

Placement needs no step: the allocation is PCM. Generated names derive from the names of what
they complete, and every step runs in name order, so the result does not depend on the order of
the instances.

Offered loads (`src/load.rs`) come from a walk over the PCM model with expected values, not from
a simulation; see D11.

Modularity (`translate.rs` `modularity`, DESIGN D32, D44) is the cohesive MQ of msa-sim-service's
round 2, made generic: the modules are the components with a `Placement` that may sit outside the
`RemoteCalls.direct` containers; each module's executions per time unit count as cohesion of its
container; a call between two modules in different containers couples them by its frequency times
its mechanism's `couplingWeight`, else 2 (bypass), 1 (sync, cached, a mechanism of another layer),
0.5 (composed call) or 0.3 (async). A redirected call, or one that bypasses its callee across
containers, does not run its callee. `ARCH_DEBUG_MQ=1`
prints every cluster's intra and inter weight to stderr.

## Verification

`cargo test --release` (40 tests: the 34 below and the 6 of `tests/dynamic.rs`, which check the
dynamic translation (the static model unchanged, one pod behind a load balancer, the SPD and HPA
files), the HPA's steady replicas at a constant load against the ideal autoscaler (2, 3, 5 pods),
a load step's SLO violations, lag and scale-in, the cap on created pods with its penalty, and the
cache; `tests/fixtures/chain.m` is a gateway → frontend → backend chain
with a database, a read side, a notifier and the placeholders the P6 tests fill in):

| Test | Checks |
| --- | --- |
| `remote_call_costs_exactly_what_remote_calls_says` | stub and skeleton demands equal the configured means; offered loads move by exactly λ·cost; simulated 24 ms = 5 + 2 + 4 + 3 + 10 (±2 %); co-located calls get nothing |
| `cached_call_gives_the_expected_mean` | 5 + 0.75·1 + 0.25·10 = 8.25 ms (±2 %), loads exact |
| `async_call_runs_in_the_background` | 5 ms; the callee's load stays |
| `async_remote_call_goes_through_the_broker` | publish and deliver CPU, broker load, 5.5 ms for the caller (the publish is on its path) |
| `replicas_use_the_limit_speed_rule` | (2, 0.5) → 2 cores at 500; (1, 1.5) → 2 at 750; 25 ms simulated |
| `node_slowdown_matches_msa_sim_service` | the formula equals msa-sim-service's on a grid; the translated rates equal 1000/s |
| `translation_is_deterministic_and_order_independent` | every decision kind at once; shuffled instances and reference lists translate to the same model, cost, modularity and node loads |
| `models_without_decisions_simulate_as_in_palladio_sim_service` | the fixture and the six Palladio Simulation models: same PCM digest and identical outcome |
| `redirected_call_reads_a_copy_kept_with_the_caller` | a copy as the HPA project's sync realises it (copy store with a Colocation, its operations' Work, a Redirect, a published Notification): read and write demands; 5 + 1 ms simulated; the store's HDD load with its host |
| `cost_and_modularity` | cost from the CostModel; no Placement, no cluster |
| `modularity_is_cohesive_mq_over_movable_components` | cohesive MQ: two containers 2 × 2/3, one container 1, the async coupling weight and a mechanism's `couplingWeight` (0.5; absent for another layer's class: 1), a cache couples as the call, components that can only sit in direct containers are left out, a redirected call does not run the callee |
| `direct_containers_get_no_middleware` | no stub or skeleton into a direct container: 15 ms |
| `sidecar_cores_per_replica` | 2 replicas × 3 sidecar cores = 6 cores at full speed |
| `container_prices_replace_the_cost_rule` | a priced container costs its price, and nothing when it hosts nothing |
| `work_becomes_demands_at_the_rates_of_its_container` | `Work` on the store's CPU and HDD: 5 + 2 + 1 ms, loads; twice the units on a CPU twice as fast; nothing for a component without an assembly |
| `a_notification_delivers_in_the_background` | 1 ms CPU on the path, the delivery as a plain call in the background (25 ms no-load, the request does not wait for it), its stub's and callee's load; `published` without a broker is the same |
| `colocation_follows_its_host` | the colocated assembly's load moves with its host; the walks before translation do not see it; a Colocation of an allocated assembly is an error |
| `chain_baseline` | the chain, all synchronous and apart: 70 ms analytic and simulated |
| `composed_call_moves_to_the_composer` | 71.5 ms (analytic, simulated), the loads per container, the composer's cache, inline in one container, coupling 0.5 |
| `bypass_does_the_callees_work_and_reaches_its_database` | 58 ms, the callee's other call skipped, loads, plain in one container, coupling 2 and no callee execution |
| `orchestration_moves_the_steps_to_the_trigger` | choreographed 70 ms, orchestrated 71 ms; asynchronous steps: 43.5 / 43 ms, request and reply loads on broker and both sides; a step with the orchestrator inline |
| `read_store_is_fed_by_projection` | a projection as an unpublished Notification (the sync makes it while the read side sits apart): +0.5 ms on the path, the apply load on the read store; none: nothing changes |
| `quota_runs_at_full_speed_when_idle` | quota speed 0.8 at u = 0.2 (q 0.5) and 1 − 0.2/3·0.25 (q 2.4), utilisation on the capacity, no-load time at full speed |
| `a_write_publishes_once_for_all_its_copies` | a published Notification with two Deliveries: one publish, two broker messages, deliver and write per copy |
| `autoscaler_derives_replicas_from_offered_load` | 1 core of load, 0.5-core replicas, target 0.7: 3 replicas, utilisation 2/3, the same PCM as 3 fixed replicas, cost; exactly at the target no extra replica; idle 1; target 0.5: 4; `maxReplicas` caps with a warning |
| `autoscaler_cost_over_load_levels` | levels 0.5 × (weight 3) and 2 × (weight 1): 2 and 6 replicas, cost on their weighted mean, 3 simulated; a priced container per replica |
| `failure_share_follows_the_synchronous_path_with_shared_fate` | the chain with four change units: unavailabilities 0.011, 0.026, 0.012 (a database server: u_db + κ·unit), 0.006; failure 1 − Π(1 − u) with everything synchronous, the same with sidecars, without the asynchronous notify's container, with a quarter of it behind a cache; one process term when three components share a container |
| `change_coupling_counts_containers_touched_per_change` | (10·1 + 20·1.15 + 5·1 + 5·1.15)/40; a bypass with break probability 0.6; co-located callers break nothing |
| `inconsistency_counts_stale_reads_and_remote_writes` | two remote writes 2; asynchronous 1 + 1.5; co-located 1; a cache 0.75 more; a `Replica` apart 0.25 more for the misses |
| `stale_reads_of_a_mechanism_count_across_containers` | `staleReads` 1 and 0.4 of a redirected query across containers; nothing in one container; a redirected query no longer reads the Replica apart |
| `latency_budget_and_peak_capacity` | 70 ms against 50 ms: 0.4; against 100 ms: 0; peak utilisation 10 × 0.008 and 200 × 0.008 |
| `autoscaled_cost_at_a_peak_level` | O1: cost with replicas at 10 × the load, the simulated model at 1 replica |
| `autoscaler_counts_the_sidecar` | 3 cores of sidecar load → 5 replicas, 5 sidecar cores; a Deployable without replicas or autoscaling is an error |
| `functions::tests` | the manifest declares every function and record |

In the workbench, `scripts/evaluate.fn` `report()` gives the same numbers as `eval` on the
command line (business-trip: 7.4085396027556705 s, 158 287 events, identical to SimuLizar-exact
runs of the original PCM files; MediaStore: 23 554.228 ms for download).

## Layout

| Path | Contents |
| --- | --- |
| `src/translate.rs` | Decisions, the completion rules, cost and modularity |
| `src/load.rs` | Offered loads and call frequencies of a PCM model |
| `src/graph.rs` | The model as an editable instance graph (keeps opposite ends) |
| `src/mfile.rs` | `.m` reader |
| `src/functions.rs` | Manifest and function calls |
| `src/session.rs`, `src/server.rs` | Sessions and HTTP/WebSocket, adapted from palladio-sim-service |
| `src/dynamic.rs` | Dynamic runs with the HPA: SPD and HPA files, the run, the dynamic objectives, their cache |
| `tests/translation.rs`, `tests/dynamic.rs`, `tests/fixtures/two-tier.m` | The tests |

## Limitations

- Composite components are not walked by the load analysis (a warning says so).
- A redirected call loses its parameter and return usages.
- `Work` uses the rates of the container of its component's first assembly.
- Several assemblies of one component type on containers with different CPU rates share one
  cache-hit demand (D9).
- Rule 0 works on a component's first assembly: the roles it adds are wired from every assembly,
  but co-location and the composer's container are judged by the first one.
- A bypass copies the CPU of the callee's top-level internal actions only; demands that depend on
  parameters, and calls inside loops or branches, are left out (with a warning for the former).
- An orchestrated step whose loop or branch also holds other calls moves them along, on the
  consumer's roles; the MiSArch steps have none.

The MiSArch models of the project (`models/misarch-*.m`, 2 551 objects since P6) are read,
translated and simulated (22 000 requests) by `eval` in about 0.5 s; `validation/msa-comparison.md`
there compares them with msa-sim-service (within 1 % for today's cut and round 2's knee at the
design load, identical no-load times).
