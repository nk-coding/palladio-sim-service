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
the time of each step (MiSArch: read 9 ms, translation 65–75 ms, compilation 50–60 ms, simulation
of 22 000 measurements 60–65 ms).
`SIM_DUMP_DIR` also receives every model as the platform sent it (`received-*.txt`).

## Functions

| Function | Returns |
| --- | --- |
| `archSimulate(seed = 1, maxMeasurements = 10000, maxSimTime = -1, replications = 1, simulateLinkingResources = false)` | `ArchResult`: what the monitors measured on the translated model (fields as palladio-sim-service's `SimResult`) |
| `archStat(element, metric = "Response Time", statistic = "mean", ...)` | one number, as `pcmStat` |
| `archCost()` | cost from the `CostModel` (DESIGN §12 D13; `ContainerPrice`s, D22) |
| `archModularity()` | cohesive MQ of the call graph between the movable components, containers as clusters (D32) |
| `archUtilisation(element = "")` | offered load: of a container (its highest resource), of `node:<name>`, or the highest of all |
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
| 0 | `CallSite` with a `LocalCopy`, `CopyFeed` | a copy-store component with the store; the call reads the copy (`readCpuMs` CPU, `readDbMs` HDD); each writer gets an asynchronous fork that writes every copy it feeds, except a writer in the store's own container (D21) |
| 1 | `AsyncCall` | the call inside a `ForkAction` with an asynchronous forked behaviour; through a broker, on a role of its own |
| 1 | `CachedCall` | a probabilistic branch: hit = CPU `hitCpuMs` on the caller, miss = the call |
| 2 | `RemoteCalls` | per assembly connector between two containers: a stub with the caller (CPU, [broker], latency, call) and a skeleton with the callee (CPU, call); one SEFF per signature, forwarding parameters and return values. Connectors into a `direct` container are left alone (D19); a sidecar has `sidecarCores` per replica (D20) |
| 3 | `Deployable` | n replicas of q cores: `numberOfReplicas` = n·⌈q⌉, `processingRate` × q/⌈q⌉ |
| 4 | `Node` | slowdown s = 1 + C(c, U)/(c − U) from the node's CPU load U; every CPU on the node runs at most at 1/s of full speed |

Placement needs no step: the allocation is PCM. Generated names derive from the names of what
they complete, and every step runs in name order, so the result does not depend on the order of
the instances.

Offered loads (`src/load.rs`) come from a walk over the PCM model with expected values, not from
a simulation; see D11.

Modularity (`translate.rs` `modularity`, DESIGN D32) is the cohesive MQ of msa-sim-service's
round 2, made generic: the modules are the components with a `Placement` that may sit outside the
`RemoteCalls.direct` containers; each module's executions per time unit count as cohesion of its
container; a call between two modules in different containers couples them by its frequency times
1 (sync, cached), 0.5 (local copy) or 0.3 (async). A call that reads a local copy does not run its
callee. `ARCH_DEBUG_MQ=1` prints every cluster's intra and inter weight to stderr.

## Verification

`cargo test --release` (17 tests):

| Test | Checks |
| --- | --- |
| `remote_call_costs_exactly_what_remote_calls_says` | stub and skeleton demands equal the configured means; offered loads move by exactly λ·cost; simulated 24 ms = 5 + 2 + 4 + 3 + 10 (±2 %); co-located calls get nothing |
| `cached_call_gives_the_expected_mean` | 5 + 0.75·1 + 0.25·10 = 8.25 ms (±2 %), loads exact |
| `async_call_runs_in_the_background` | 5 ms; the callee's load stays |
| `async_remote_call_goes_through_the_broker` | publish and deliver CPU, broker load, 5 ms for the caller |
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

The MiSArch models of the project (P3, `models/misarch-*.m`, 2 275 objects) are read, translated
and simulated (22 000 requests) by `eval` in 0.2 s; `validation/msa-comparison.md` there compares
them with msa-sim-service.
