# Palladio Simulation plugin service

An mdeo plugin service, written in Rust, that gives scripts two external functions:
`pcmSimulate()` and `pcmStat()`. They simulate the Palladio (PCM) model the script runs on with
[SimOxide](../simoxide) in its exact mode. For the supported model subset, a run reproduces
SimuLizar 5.2.2 byte for byte: the same random numbers, the same events and the same measurements.

The service speaks the platform's `script-functions` protocol natively (CBOR over the session
WebSocket, with RS256 session tokens checked against the backend's JWKS). No Kotlin module is
needed.

## Run it

```bash
./run.sh              # build (release), then serve on http://localhost:3020
./run.sh --no-build   # start the last build
```

Then:

1. Register `http://localhost:3020/` as a plugin (**Settings → Plugins**, admin).
2. Enable **Palladio Simulation** in a project.

In the dev stack, the service runs in the `palladio` window of the `mdeo-dev` tmux session.

| Variable | Default | Meaning |
| --- | --- | --- |
| `PORT`, `HOST` | `3020`, `0.0.0.0` | Where to listen |
| `BACKEND_API_URL` | `http://localhost:8080/api` | The backend; session tokens are verified against `<url>/.well-known/jwks.json` |
| `JWT_ISSUER` | `mdeo-platform` | Required token issuer |
| `MAX_SESSIONS` | `256` | Sessions open at once (every optimizer worker holds one); more are refused with 4503 |
| `SIM_THREADS` | one per CPU | Simulation threads, shared by all sessions and all replications |
| `SIM_TIMEOUT_SECONDS` | `600` | Wall-clock limit of one run (0: none) |
| `SIM_MAX_EVENTS` | `0` | Event limit of one run (0: none) |
| `SIM_MAX_REPLICATIONS` | `10000` | Most replications one call may ask for |
| `SIM_MODEL_CACHE_SIZE` | `256` | Compiled models kept, by content |
| `SIM_RESULT_CACHE_SIZE` | `4096` | Simulation outcomes kept, by model content and run parameters |
| `SIM_DUMP_DIR` | unset | Write every received model there (`model.m` and the PCM files it becomes), for debugging |
| `RUST_LOG` | `info` | Log level |

## The functions

```fn
using "../palladio.mm"

fun report(): string {
    val r = pcmSimulate()                     // one run: seed 1, stop after 10 000 measurements
    var out = ""
    for (s in r.series) {
        out = out + s.kind + " " + s.elementName + " | " + s.metric + " | mean=" + s.mean + "\n"
    }
    return out
}

// An objective: mean response time of a usage scenario (by instance or entity name).
fun responseTime(): double {
    return pcmStat("defaultUsageScenario", maxMeasurements = 2000)
}
```

Both functions take the run parameters `seed = 1`, `maxMeasurements = 10000`,
`maxSimTime = -1` (seconds; `-1` means none), `replications = 1` and
`simulateLinkingResources = false`. A run needs a stop condition: `maxMeasurements`, `maxSimTime`,
or both, as in SimuLizar.

**`pcmSimulate(...): SimResult`** reports everything the model's monitors measured.
- A `SimResult` has `series: ReadonlyList<SimSeries>`, `replications`, `seed`, `events`,
  `measurements` and `simulatedTime`.
- A `SimSeries` has:
  - `measuringPoint`: the key as in SimuLizar's `measurements.csv`;
  - `kind`: `UsageScenario`, `ActiveResource`, ...;
  - `element` and `elementName`: the measured instance, and its entity name;
  - `monitor`;
  - `metric`, e.g. `Response Time`;
  - `count`, `mean`, `stdDev`, `min`, `max`, `p50`, `p90`, `p95`, `p99`;
  - `timeWeightedMean`, for state and utilisation series;
  - `ciHalfWidth`: the 95 % confidence half-width of the mean over the replications.

**`pcmStat(element, metric = "Response Time", statistic = "mean", ...): double`** returns one
number.
- `element` names what was measured, in any of these forms:
  - the instance name or entity name of the measured element;
  - a monitor's instance or entity name;
  - a whole measuring point key.
- `metric` is matched loosely: `Response Time`, `RESPONSE_TIME` and `Response Time Tuple` are the
  same.
- `statistic` is one of `mean`, `stdDev`, `min`, `max`, `p50`/`median`, `p90`, `p95`, `p99`,
  `count`, `throughput`, `timeWeightedMean` or `ciHalfWidth`.

**Replications.** `replications = n` runs seeds `seed … seed+n-1` in parallel and pools their
tuples. With one replication, every statistic is exactly that of the single SimuLizar run.

**Sharing and caching.**
- Calls on the same model with the same run parameters share one simulation. An optimizer
  evaluating several objectives of one candidate simulates it once.
- Results are cached by model content, so a model met again, in any session, is not simulated
  again.
- Both are safe because exact mode is deterministic.

**Errors.** A model SimuLizar would reject, or a run SimuLizar would abort, fails the call with
the reason, which the script sees.

## Parallelism

- **Simulations** run on one rayon pool (`SIM_THREADS`), never on the async runtime. Calls of
  all sessions and the replications of one call spread over the same threads.
- **Sessions** are independent; each handles its calls in order, as the protocol requires.
- **Keepalive.** Every session pings every 30 s and drops a peer silent for 90 s, and keeps
  answering while a long simulation runs.

Measured through the protocol, on a 22-core machine: 22 sessions × 8 calls of the SimuLizar
mediastore example at 10 000 measurements each, every call with a seed of its own.

| Threads | 1 | 8 | 22 |
| --- | --- | --- | --- |
| Runs/s | 19.9 | 126 | 210 |

```bash
SIM_THREADS=22 cargo test --release --test session throughput -- --ignored --nocapture
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

## Verification

| Check | Result |
| --- | --- |
| `palladio-sim-service roundtrip <dirs>`: PCM → mdeo → PCM (with the platform's name-sorted lists), simulated, compared with SimOxide on the original files and with SimuLizar's own `measurements.csv` | 66/66 corpus models byte-identical. 101/103 including the fuzz and regression fixtures: one fixture's stored CSV comes from tape-replay mode, and one relies on two files having objects with equal ids, which mdeo names cannot express |
| `roundtrip --readable` (instances renamed after entity names) | 66/66, every value identical |
| `cargo test --release` | Protocol decoding of the specification's reference bytes; end-to-end sessions with self-signed RS256 tokens: results bit-identical to direct runs, `pcmStat`, replications, refusal codes 4401/4404/4409, `unknown-model`, 48 parallel sessions |
| `tests/kotlin_compat.rs` against the platform's kotlinx-serialization classes | Encodes and decodes both ways |
| Through the platform: `scripts/simulate.fn report()` on the six models of the Palladio Simulation project | All 79 series identical to direct simulation of the original PCM files |

## Layout

| Path | Contents |
| --- | --- |
| `src/schema.rs` | The Palladio metamodel as mdeo sees it, derived from the PCM Ecore tables |
| `src/mm.rs`, `src/mtext.rs` | Writers for `.mm` (split into parts) and `.m` |
| `src/import.rs` | PCM XMI → mdeo model |
| `src/xmi.rs` | mdeo model → PCM XMI |
| `src/engine.rs` | Compiling, running, replications, caches, the thread pool |
| `src/functions.rs` | The functions: manifest payload and operations |
| `src/protocol.rs`, `src/session.rs`, `src/server.rs`, `src/auth.rs` | `script-functions` protocol, sessions, HTTP/WebSocket, token verification |
