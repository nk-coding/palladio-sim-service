# Palladio Architecture plugin service (`palladio-arch-service`)

An mdeo plugin service, written in Rust. It evaluates Palladio (PCM) models that carry
architecture decisions (`architecture.mm`, `microservices.mm` of
[palladio-architecture-optimization](../palladio-architecture-optimization)). For the model a
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
| `archNoLoadTime(exclude = "")` | the rate-weighted mean response time without contention over the usage scenarios (but those named in `exclude`, comma-separated), in model time units: the expected-value walk of the translated model, no simulation (DESIGN D48; static objectives) |
| `archInfo()` | `ArchInfo`: translated or not, cost, modularity, every resource's load, warnings |

A model without decisions (or with decisions that change nothing) is simulated exactly as
palladio-sim-service simulates it. Simulations are shared and cached as there.

## Command line

```text
palladio-arch-service translate MODEL OUT_DIR    OUT_DIR/<name>.translated.m (plain PCM, formatted with the
                                                 project's tools/format_model.py) and OUT_DIR/pcm/ (PCM XMI)
palladio-arch-service eval MODEL [--seed N] [--measurements N] [--max-sim-time S] [--replications N]
                                 [--plain] [--json]
                                                 translate, simulate, print series, loads, cost, modularity
palladio-arch-service serve                      the plugin service (the default)
```

`MODEL` is an mdeo `.m` file. `--plain` drops the decisions; `--json` prints machine-readable
output for comparisons (e.g. with `msa-sim eval`).

## Translation

Applied to a copy of the model, in this order (`src/translate.rs`):

| Step | Decision | PCM result |
| --- | --- | --- |
| 0 | `Workflow` with an `Orchestrated` style | every step (the call with its loop or branch) leaves its SEFF; the trigger's component makes them at the end of the trigger's SEFF, `stepCpuMs` each, as a forest (parallel siblings) or level by level; an asynchronous step to another container is a request and a reply through the broker; all in the background when the first step is asynchronous and remote (D42) |
| 0 | `CallSite` with a `Bypass` | across containers: the callee's top-level CPU work on the caller, `latencyMs`, and the callee's calls into `direct` containers, made by the caller (D40) |
| 0 | `CallSite` with a `ComposedCall` | across containers: the call (with its loop or branch) moves after the composer's call into the caller: `composeCpuMs`, then the composer's cache branch or the call; several in parallel (D39) |
| 0 | `CallSite` with a `LocalCopy`, `CopyFeed` | a copy-store component with the store; the call reads the copy (`readCpuMs` CPU, `readDbMs` HDD); each writer gets an asynchronous fork that publishes once (a publisher component on its sidecar) and sends every copy it feeds a message, except a writer in the store's own container (D21, D43); a feed without writers is a copy carried by other messages (D46) |
| 0 | `Projection` | when `reads` and `source` sit in different containers: after every writer, `cpuMs` on its CPU and an asynchronous call of `apply` on `reads` (D41) |
| 1 | `AsyncCall` | the call inside a `ForkAction` with an asynchronous forked behaviour; through the broker to another container, no fork here: the stub publishes on the caller's path and forks the rest (D43) |
| 1 | `CachedCall` | a probabilistic branch: hit = CPU `hitCpuMs` on the caller, miss = the call |
| 2 | `RemoteCalls` | per assembly connector between two containers: a stub with the caller and a skeleton with the callee; one SEFF per signature, forwarding parameters and return values. Plain call: caller CPU, latency, call / callee CPU, call. Asynchronous: publish, then broker, latency, call in the background / deliver, call. Copy update: broker, latency, call / deliver, call. Orchestrated request: publish, broker, latency, call, broker, latency, deliver / deliver, call, publish (D43). Connectors into a `direct` container are left alone (D19); a sidecar has `sidecarCores` per replica (D20) |
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
2 (bypass), 1 (sync, cached), 0.5 (composed call, local copy) or 0.3 (async). A call that reads a
local copy, or bypasses its callee across containers, does not run its callee. `ARCH_DEBUG_MQ=1`
prints every cluster's intra and inter weight to stderr.

## Verification

`cargo test --release` (24 tests; `tests/fixtures/chain.m` is a gateway → frontend → backend chain
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
| `local_copy_reads_the_copy_and_writers_feed_it` | read and write demands; 5 + 1 ms simulated |
| `cost_and_modularity` | cost from the CostModel; no Placement, no cluster |
| `modularity_is_cohesive_mq_over_movable_components` | cohesive MQ: two containers 2 × 2/3, one container 1, async and copy coupling weights, a cache couples as the call, components that can only sit in direct containers are left out, a copy read does not run the callee |
| `direct_containers_get_no_middleware` | no stub or skeleton into a direct container: 15 ms |
| `sidecar_cores_per_replica` | 2 replicas × 3 sidecar cores = 6 cores at full speed |
| `container_prices_replace_the_cost_rule` | a priced container costs its price, and nothing when it hosts nothing |
| `local_copy_read_cpu_runs_in_the_store` | `readCpuMs` on the store's CPU: 5 + 2 + 1 ms |
| `a_writer_in_the_stores_container_feeds_nothing` | one message for a remote writer, none for a co-located one |
| `chain_baseline` | the chain, all synchronous and apart: 70 ms analytic and simulated |
| `composed_call_moves_to_the_composer` | 71.5 ms (analytic, simulated), the loads per container, the composer's cache, inline in one container, coupling 0.5 |
| `bypass_does_the_callees_work_and_reaches_its_database` | 58 ms, the callee's other call skipped, loads, plain in one container, coupling 2 and no callee execution |
| `orchestration_moves_the_steps_to_the_trigger` | choreographed 70 ms, orchestrated 71 ms; asynchronous steps: 43.5 / 43 ms, request and reply loads on broker and both sides; a step with the orchestrator inline |
| `read_store_is_fed_by_projection` | nothing while the read side sits with the write side; apart: +0.5 ms projection, apply load on the read store |
| `quota_runs_at_full_speed_when_idle` | quota speed 0.8 at u = 0.2 (q 0.5) and 1 − 0.2/3·0.25 (q 2.4), utilisation on the capacity, no-load time at full speed |
| `a_write_publishes_once_for_all_its_copies` | one publish for two copies, two broker messages, deliver and write per copy |
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
| `tests/translation.rs`, `tests/fixtures/two-tier.m` | The tests |

## Limitations

- Composite components are not walked by the load analysis (a warning says so).
- A call with a local copy loses its parameter and return usages.
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
