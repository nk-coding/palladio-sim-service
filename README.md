# Palladio Architecture plugin service (`palladio-arch-service`)

An mdeo plugin service, written in Rust, for the architecture optimisation of Palladio (PCM)
models ([palladio-architecture-optimization-domain](../palladio-architecture-optimization-domain)).
It knows PCM only: the architecture layers above it are realised in the model by model
transformations. Its script functions (`pcm*`) analyse the plain PCM part of the model a script
runs on (offered loads, no-load times, call rates, failures) and simulate it with
[SimOxide](../simoxide), also over load profiles and with Kubernetes-style autoscaling, through
[palladio-sim-service](../palladio-sim-service), which it uses as a library (protocol, auth, PCM
schema, XMI writer, simulation engine). The command line adds the design detail of a model and
the layer-2 translation the domain project checks its realisation against.

The `arch*` and `archHpa*` functions of the earlier projects (layer-2 decisions translated by the
plugin, `eval` and `hpa` commands) were removed on 2026-10-22; tag `pre-cleanup` has them.

## Run it

```bash
./run.sh              # build (release), then serve on http://localhost:3022
./run.sh --no-build   # start the last build
```

Register `http://localhost:3022/` under **Settings → Plugins** and enable **Palladio
Architecture** in a project. The environment variables are those of palladio-sim-service
(`SIM_THREADS`, `SIM_DUMP_DIR`, ...), except that the default port is 3022. Cap the caches for
large models: a compiled MiSArch model (9 200 PCM objects) takes about 10 MB, so the default
`SIM_MODEL_CACHE_SIZE=256` can hold about 2.5 GB; the dev stack runs with
`SIM_MODEL_CACHE_SIZE=16 SIM_RESULT_CACHE_SIZE=1024`. `SIM_DUMP_DIR` also receives every model as
the platform sent it (`received-*.txt`). The domain project's harness runs a copy of
`target/release/palladio-arch-service` (`tools/run-domain.sh`).

## Functions

Plain PCM in, numbers out: they read the model's PCM part as it is (instances of other classes
are ignored) and know nothing above PCM; whatever says how to read the model (overrides, branch
probabilities, unavailabilities, transparent assemblies, load profiles, scaling groups, budgets)
comes from the script. Used by the domain project (`palladio-architecture-optimization-domain`,
DESIGN.md §B and §H), whose layer 2 is realised in the model by transformations: its
`transform/metrics.fn` calls `pcmResources`, `pcmScenarios`, `pcmOperations`, `pcmCallRates`,
`pcmFailure`, `pcmSourceFailure`, `pcmMean` and `pcmSimulate`, its explorer the design detail.
Times in model time units unless a name says `Ms`/`Seconds`.

| Function | Returns |
| --- | --- |
| `pcmResources()` | `PcmResource` per processing resource: container, kind, rate, replicas, demand, busy, utilisation |
| `pcmScenarios()` | `PcmScenario` per usage scenario: arrival rate, no-load response time |
| `pcmOperations()` | `PcmOperation` per entry-level system call: signature, callee, calls per time unit, no-load time per call |
| `pcmWeightedVisits(actions, weights, exclude = "")` | rate-weighted mean over the scenarios of Σ expected executions per request × weight |
| `pcmOperationVisits(actions, weights)` | `PcmOperationValue` per entry-level call: Σ expected executions per call × weight (the call itself counts once) |
| `pcmCallRates(transitions, probabilities)` | `PcmCall` per external and entry-level call: calls per time unit, branch probabilities overridden |
| `pcmFailure(containers, unavailabilities, transparent, exclude = "")` | rate-weighted share of requests needing an unavailable container on their synchronous path |
| `pcmSourceFailure(containers, sources, unavailabilities, transparent, exclude = "")` | rate-weighted share of failed requests with failure sources (common causes): source s (`unavailabilities[s]`) takes down the containers `containers[i]` with `sources[i] = s`; sources are independent, a request fails with 1 − Π_s (1 − P(touch any container of S_s) · u_s), P by `pcmFailure`'s walk with S_s as one node (one walk for all sources). One source per container is `pcmFailure` |
| `pcmOperationFailure(containers, unavailabilities, transparent)` | `PcmOperationValue` per entry-level call: the probability that one call fails so |
| `pcmSimulate(overrides, ...)`, `pcmStat(overrides, element, metric, statistic, ...)` | a simulation with processing resources' replicas and rates overridden (`PcmOverride`) |
| `pcmRun(overrides, profile, usageStreams = false, ...)`, `pcmRunStat(...)` | the same with a per-scenario load profile (`PcmLoadStep` {scenario or null = every scenario, time, factor}; SimOxide LP-1) and SimOxide's usage streams (common random numbers, RND-11) |
| `pcmHpaRun(overrides, groups, services, profile, budgetElements, budgetsMs, warmUp, end, msPerTimeUnit, sloFactor, usageStreams = false, seed = 1, maxPods = 400)` | `PcmHpaResult`: a dynamic run with SPD and SimOxide's HPA: per `PcmScalingGroup` (container = the pod, min/max/initial replicas, target utilisation, start-up, scale-down window (< 0: default), CPU request (≤ 0: the pod's cores)) an elastic infrastructure, per `PcmServiceGroup` (assembly, its load balancer) a service group (a group without any gets load balancers from the plugin, Slingshot's pattern: one branch with one call per pod, on a container `LoadBalancerServer` without work); replica-seconds, peak, rescales, created pods, flapping, the share over `sloFactor` × budget of the budgeted elements (scenarios, calls, or signatures as system operations), mean and p95 of the calls; cached per model and request |
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
dynamic run. A run that creates more than `maxPods` pods (default 400) stops and is reported as
capped.

## Command line

```text
palladio-arch-service serve                      the plugin service (the default)
palladio-arch-service detail MODEL [REQUEST.json] [--out FILE]
                                                 the design detail of a model's PCM (above)
palladio-arch-service oracle MODEL... [--exclude A,B]
                                                 the translation's analytic values (cost, modularity,
                                                 utilisation, replicas, no-load times, failure, change
                                                 coupling, inconsistency, latency and peak violations) of
                                                 each model, one JSON object per line
palladio-arch-service translate MODEL OUT_DIR    OUT_DIR/<name>.translated.m (plain PCM, formatted with the
                                                 project's tools/format_model.py) and OUT_DIR/pcm/ (PCM XMI)
```

`MODEL` is an mdeo `.m` file or, for `detail` and `oracle`, mdeo model data (`.json`).
`--exclude` names the usage scenarios the rate-weighted values leave out (default `login`).

## Translation

The completion of layer 2 (`architecture.mm`) into plain PCM, with its analytic cost,
modularity and quality values: what the `arch*` functions evaluated. It stays as the reference
the domain project checks its realisation by model transformations against (`oracle`,
`translate`; DESIGN.md B3 there). The D numbers are the decisions in §12 of the archived
project's DESIGN.md (`../old/palladio-architecture-optimization`). Applied to a copy of the
model, in this order (`src/translate.rs`):

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

`cargo test --release` (52 tests, one more ignored; `tests/fixtures/two-tier.m` is a frontend →
backend system, `tests/fixtures/chain.m` a gateway → frontend → backend chain with a database, a
read side, a notifier and placeholders; the MiSArch tests read the domain project's
`models/misarch-today.m` and are skipped without it):

| Tests | Check |
| --- | --- |
| `tests/pcm_ops.rs` | per-operation analyses (visits, failures, no-load times) against the per-scenario ones, also on MiSArch; per-scenario load profiles and usage streams (deterministic, common random numbers across designs, the runner's cache); the design detail (counts, histograms, budgets, utilisation against the analysis); PMF inputs enumerated exactly; the script functions as the platform calls them; dynamic runs: load balancers added by the plugin give the same run as given ones, the pods follow the load, settle at the ideal count at a constant load (1, 2, 3 pods), and creation is capped |
| `tests/pcm_sources.rs` | `pcmSourceFailure`: a dedicated database counts a release once, a shared server with every tenant's, branches and chains mark a set once, one source per container is `pcmFailure` (also on MiSArch); `cost_per_call` (ignored) times both |
| `tests/pcm.rs` | on the translated MiSArch model the `pcm*` analyses give the translation's offered loads and failure share |
| `tests/translation.rs` | the translation rules above: demands and simulated response times per mechanism, replicas, quota, node slowdown, autoscaler counts and cost, cost, modularity, failure, change coupling, inconsistency, latency budget and peak capacity; deterministic and order independent; models without decisions simulate exactly as in palladio-sim-service |
| unit tests | the manifest declares every function and record; the design detail's step means and request parsing |

## Layout

| Path | Contents |
| --- | --- |
| `src/functions.rs` | Manifest and function calls |
| `src/pcm.rs` | The analyses of the `pcm*` functions (per model, cached) |
| `src/pcmrun.rs` | Simulations with load profiles and usage streams |
| `src/pcmhpa.rs`, `src/dynamic.rs` | Dynamic runs: the request and outcome; the SPD and HPA files and the run |
| `src/detail.rs` | The design detail |
| `src/load.rs` | Offered loads and call frequencies of a PCM model |
| `src/translate.rs`, `src/translate/quality.rs` | The layer-2 translation, cost, modularity and quality values (the oracle); the load balancers of the dynamic runs |
| `src/graph.rs` | The model as an editable instance graph (keeps opposite ends) |
| `src/mfile.rs` | `.m` and model data reader |
| `src/session.rs`, `src/server.rs` | Sessions and HTTP/WebSocket, adapted from palladio-sim-service |
| `tests/` | The tests and their fixtures |

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
