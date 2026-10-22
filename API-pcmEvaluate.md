# `pcmEvaluate`: simulate the model's PCM with SimOxide, return what was measured

Status 2026-10-22, revision 2 (generic). The plugin does one thing: it runs **one SimOxide
simulation** of the plain PCM part of the model the script runs on and returns the raw
measurements of that run. It interprets nothing: cost, failure models, budgets, consistency,
which percentile matters are the scripts' (`.fn`). It is the only script function; there is no
`pcmMean` and no analysis any more.

Changes against revision 1: `sourceContainers`/`sourceIds`, `sources`, `sourceSets` replaced by
`pathSets` (raw container sets per scenario); response times as requested percentiles plus a
log histogram plus per-bin series; `containers` renamed `resources` with per-bin series; every
setting is an argument with a default; `wallSeconds`, `maxEvents` limits; `timedOut`;
`version`.

## Invariance to names and order

Results do not depend on element names, ids or the order of instances and lists: before
simulating, the plugin brings the PCM into a canonical form (`src/canon.rs`: colour refinement
of the instance graph by class, attribute values and references, entity names left out; names,
instance order and every reference list then follow from the structure). Two models that differ
only in names and order are simulated identically and hit the same cache entry; the result
refers to the caller's own instances, and its lists come in the canonical order (equal designs:
equal lists). Residual: instances that the structure cannot tell apart (interchangeable ones,
e.g. two identical unused containers or two identical branches) are ordered by their names;
their measurements are the same multiset either way, but which of the caller's instances
carries which value may follow the names. Deterministic in every case.

## Signature

Use **named arguments**; everything but `end` has a default.

```fn
pcmEvaluate(
    end: double,                              // s: end of the run and of the measured window
    warmUp: double = 0.0,                     // s: start of the measured window
    binSeconds: double = 10.0,                // s: bins of the per-bin series, from warmUp on
    msPerTimeUnit: double = 1000.0,           // ms per model time unit
    profile: List<PcmLoadStep>? = null,       // per-scenario load profile; null: the model's rates
    groups: List<PcmScalingGroup>? = null,    // autoscaled containers (SPD + HPA); null: none
    services: List<PcmServiceGroup>? = null,  // load balancers already in the model (rare)
    overrides: List<PcmOverride>? = null,     // replicas/rate of processing resources
    percentiles: List<double>? = null,        // in percent; null: [50, 90, 95, 99]
    detailCalls: boolean = false,             // also response times per entry-level call and assembly operation
    histogramPerDecade: int = 10,             // log-histogram bins per decade of ms
    seed: int = 1,
    usageStreams: boolean = true,             // common random numbers (SimOxide RND-11)
    fixDoubleResume: boolean = true,          // fix SimuLizar's double resume at a fork join (REF-2)
    maxPods: int = 400,                       // pods the HPA may create in all (beyond: capped)
    hpaSyncSeconds: double = 15.0,            // Kubernetes' --horizontal-pod-autoscaler-sync-period
    hpaTolerance: double = 0.1,               // --horizontal-pod-autoscaler-tolerance
    hpaDownscaleStabilisationSeconds: double = 300.0,  // default scale-down window
    hpaCpuInitialisationSeconds: double = 300.0,       // --horizontal-pod-autoscaler-cpu-initialization-period
    maxPathSets: int = 10000,                 // most distinct path sets returned per scenario
    wallSeconds: double = 60.0,               // wall-clock limit of the simulation (≤ 0: none)
    maxEvents: long = 0                       // event limit (0: the service's SIM_MAX_EVENTS)
): PcmEvaluation
```

All times are **seconds**, response times **ms**. Bins: `[warmUp + i·binSeconds, …)`, the last
one ends at `end` (possibly shorter). One simulation per distinct (model PCM, arguments): calls
with equal arguments on an equal model return the cached result (a bounded LRU,
`SIM_RESULT_CACHE_SIZE` entries), so scripts may call `pcmEvaluate` in every metric function.
Deterministic for a seed. SimOxide runs in fast mode, SimuLizar flavour.

A run that fails (a model SimOxide cannot simulate, an engine error), stops at `maxPods`, at
`wallSeconds` or `maxEvents` **does not fail the call**: `failed = true`, `message` says why,
`capped` (pod cap or event limit) or `timedOut` (wall clock) tell which; the lists then hold
what was measured until the stop (possibly nothing). Wrong arguments (a group whose container
is not a ResourceContainer) fail the call: they are script bugs.

## Argument records (positional constructor order = field order; fields with defaults)

| Record | Fields |
| --- | --- |
| `PcmOverride` | `resource` (ProcessingResourceSpecification), `replicas: int`, `rate: double` |
| `PcmScalingGroup` | `container` (the pod's ResourceContainer: its CPU = one pod's cores), `minReplicas: int = 1`, `maxReplicas: int = 10`, `targetUtilisation: double = 0.7`, `initialReplicas: int = 1`, `startupSeconds: double = 30.0`, `scaleDownSeconds: double = -1.0` (< 0: `hpaDownscaleStabilisationSeconds`), `cpuRequest: double = 0.0` (≤ 0: the pod's cores), `behaviorJson: string = ""` (a Kubernetes `spec.behavior` object as JSON, passed to SimOxide's HPA as is; times in it are model time units) |
| `PcmServiceGroup` | `container`, `assembly`, `loadBalancer` (a group without any gets the plugin's load balancers, Slingshot's pattern) |
| `PcmLoadStep` | `scenario` (UsageScenario, or null = every scenario), `time: double` (s), `factor: double`: from `time` on the scenario's arrival rate is `factor` × the model's; a named scenario's steps override the all-scenario ones; before a scenario's first step its factor is 1 |

## Result: `PcmEvaluation`

| Field | Type | Meaning |
| --- | --- | --- |
| `failed`, `capped`, `timedOut` | boolean | see above |
| `message` | string | why it failed (empty otherwise) |
| `version` | string | the plugin build (`2026-10-22+<git hash>`) |
| `windowSeconds`, `binSeconds` | double | `end − warmUp`; the bin width |
| `bins` | int | number of bins |
| `percentiles` | List<double> | the percentiles of every `percentilesMs` list, in this order |
| `simulatedSeconds` | double | simulated time reached |
| `events` | long | events of the run |
| `wallMs` | double | compute time of the simulation when it ran |
| `operations` | List<`PcmResponseStat`> | per system operation (the system's provided role × signature, a request type) |
| `sessions` | List<`PcmResponseStat`> | per usage scenario (one value per session = scenario run, think times included) |
| `calls` | List<`PcmResponseStat`> | per entry-level system call, only with `detailCalls` |
| `assemblyOperations` | List<`PcmResponseStat`> | per assembly × provided role × signature, only with `detailCalls` (operations that can call themselves are not measured) |
| `resources` | List<`PcmResourceStat`> | per processing resource of every container of the model |
| `callCounts` | List<`PcmCallCount`> | per ExternalCallAction and EntryLevelSystemCall of the model |
| `pathSets` | List<`PcmPathSet`> | per usage scenario, the distinct container sets of the sessions' synchronous paths |
| `pathSetsTruncated` | boolean | some scenario had more than `maxPathSets` sets (the most frequent are kept) |
| `createdPods`, `rescales` | long | all groups, whole run |
| `peakPods` | double | most ready pods at once, all groups, in the window |
| `reversals` | long | rescale direction reversals in the window, all groups |
| `warnings` | List<string> | e.g. load balancers added, a series that could not be read |

`PcmResponseStat` (values in ms, over the window):
`kind: string` (`systemOperation`, `scenario`, `call`, `assemblyOperation`), `key: string`
(PCM ids joined by `|`), `scenario` (UsageScenario or null), `call` (EntryLevelSystemCall or
null), `role` (OperationProvidedRole or null), `signature` (OperationSignature or null),
`assembly` (AssemblyContext or null), `count: long`, `meanMs`, `maxMs`,
`percentilesMs: List<double>` (nearest rank, one per `percentiles`),
`histogramEdgesMs: List<double>`, `histogramCounts: List<long>` (log-spaced, `histogramPerDecade`
per decade; `histogramCounts[i]` counts values in `[edges[i], edges[i+1])`; zeros get a first
bin `[0, edges[1])`), `binCounts: List<long>`, `binMeanMs: List<double>` (0 without values),
`binPercentilesMs: List<double>` (bin-major: bin `b`, percentile `j` at `b · P + j`; 0 without
values). Values are assigned to the bin of their completion time.

`PcmResourceStat` (one per ProcessingResourceSpecification of every ResourceContainer of the
model):
`container`, `resource` (the spec), `kind: string` (`CPU`, `HDD`, `DELAY`, …),
`autoscaled: boolean` (the container is a group's pod), `cores: double` (numberOfReplicas of the
spec after overrides; per pod when autoscaled),
`meanUtilisation: double` (busy instances / instances, SimuLizar's overall utilisation, time
mean over the window; autoscaled: summed over its pods, per ready pod; DELAY: 0),
`binUtilisation: List<double>`, `maxBinUtilisation: double`,
`meanReadyReplicas: double`, `peakReadyReplicas: double`, `binReadyReplicas: List<double>`,
`binReplicas: List<double>` (ready + starting pods; time means; 1 when not autoscaled),
`replicaCoreSeconds: double` (∫ (ready + starting pods) × cores dt over the window; not
autoscaled: cores × windowSeconds), `readyReplicaCoreSeconds: double`,
`rescales: long`, `createdPods: long`, `reversals: long` (its group; 0 when not autoscaled).

`PcmCallCount`: `action` (an ExternalCallAction of a SEFF, or an EntryLevelSystemCall),
`count: long`. External calls: started in the window, per action (all assembly contexts of its
component together; a per-assembly breakdown is not measured), also inside asynchronous forks.
Entry-level calls: completed in the window. Actions the plugin adds (load balancers) are left out.

`PcmPathSet`: `scenario` (UsageScenario), `containers: List<ResourceContainer>`, `count: long`:
the sessions of the scenario that ended in the window whose synchronous path used exactly these
containers. Σ count per scenario = the scenario's sessions in the window.

### Synchronous path (SimOxide `SimConfig::sync_paths`, MEAS-13)

A session is one run of a usage scenario by one user (open workload: one user; closed: one
iteration). Its containers are those of every assembly context whose SEFF executes for the
session on its synchronous path: the SEFFs the user's own process enters (entry-level system
calls and everything they call) and those of synchronous forked behaviours (a ForkAction's
synchronisation point, recursively). Asynchronous forked behaviours, and everything they start,
are off the path. A container counts once per session. A pod created by the HPA counts as its
scaled container; the plugin's `LoadBalancerServer` is not a container of the model and is left
out. Linking resources are not containers.

## Typical use (sketch)

```fn
val e = pcmEvaluate(end = 1320.0, warmUp = 120.0, msPerTimeUnit = msPerTimeUnit(),
                    profile = dayProfile(), groups = scalingGroups(), overrides = overrides())
if (e.failed) { /* infeasible */ }
// cost: Σ resources (kind == "CPU") replicaCoreSeconds × price + flat prices
// failures: per scenario, Σ pathSets count × (1 − Π_{sources s touched by the set} (1 − u_s)) / Σ count
// call rates: callCounts count / windowSeconds
// latency: operations percentilesMs[index of 95] against the budget
// capacity: resources (not autoscaled) maxBinUtilisation
```

## Design detail (explorer)

`palladio-sim-service detail MODEL REQUEST.json` / `POST /pcm/detail` take the same arguments
in JSON (`durationS` = end, `warmUpS`, `binS`, `timeUnitMs`, `profile` [{scenario?, timeS,
factor}], `groups` [{container, min, max, target, initial, startupS, scaleDownS?, cpuRequest?,
behavior?}], `services`, `overrides` [{resource, replicas, rate}], `percentiles`, `seed`,
`usageStreams`, `fixDoubleResume`, `maxPods`, `hpaSyncS`, `hpaTolerance`,
`hpaDownscaleStabilisationS`, `hpaCpuInitialisationS`, `maxPathSets`, `wallSeconds`,
`maxEvents`; the older `autoscaling` {groups, services, maxPods} is read too) plus the detail's
own `budgetsMs`, `sloFactor`, `histogramPerDecade`, `assemblyOperations`. It runs **the same
simulation** with more monitors (which do not change the run) and adds the evaluation under
`evaluation` (the `PcmEvaluation` fields as JSON, instances by id).
`palladio-sim-service evaluate MODEL REQUEST.json` prints only the evaluation.
