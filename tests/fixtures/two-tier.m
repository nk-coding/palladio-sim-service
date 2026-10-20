// A two-tier test system: Frontend.handle does 5 ms of CPU (exponential) and calls
// Backend.query, which does 10 ms. Time unit: seconds; every CPU does 1000 units per second, so
// one unit is one millisecond of CPU. Users arrive at RATE per second (replaced by the tests).

using "../../../palladio-architecture-optimization-domain/metamodel/palladio.mm"

repo : Repository { entityName = "repo" }
repo.interfaces -- IFrontend
repo.interfaces -- IBackend
repo.components -- Frontend
repo.components -- Backend

IFrontend : OperationInterface { entityName = "IFrontend" }
IFrontend.signatures -- handle
handle : OperationSignature { entityName = "handle" }

IBackend : OperationInterface { entityName = "IBackend" }
IBackend.signatures -- query
query : OperationSignature { entityName = "query" }

Frontend : BasicComponent { entityName = "Frontend" }
Frontend.providedRoles -- Frontend_provides
Frontend.requiredRoles -- Frontend_requires
Frontend.serviceEffectSpecifications -- Frontend_handle
Frontend_provides : OperationProvidedRole { }
Frontend_provides.providedInterface -- IFrontend
Frontend_requires : OperationRequiredRole { }
Frontend_requires.requiredInterface -- IBackend

Frontend_handle : ResourceDemandingSEFF { }
Frontend_handle.describedService -- handle
Frontend_handle.steps -- Frontend_handle_start
Frontend_handle.steps -- Frontend_handle_cpu
Frontend_handle.steps -- Frontend_handle_callQuery
Frontend_handle.steps -- Frontend_handle_stop
Frontend_handle_start : StartAction { }
Frontend_handle_cpu : InternalAction { entityName = "cpu" }
Frontend_handle_cpu.resourceDemand -- Frontend_handle_cpu_demand
Frontend_handle_cpu_demand : ParametricResourceDemand {
    specification    = "Exp(0.2)"
    requiredResource = ProcessingResourceType.CPU
}
Frontend_handle_callQuery : ExternalCallAction { }
Frontend_handle_callQuery.role -- Frontend_requires
Frontend_handle_callQuery.calledService -- query
Frontend_handle_stop : StopAction { }
Frontend_handle_start.successor -- Frontend_handle_cpu
Frontend_handle_cpu.successor -- Frontend_handle_callQuery
Frontend_handle_callQuery.successor -- Frontend_handle_stop

Backend : BasicComponent { entityName = "Backend" }
Backend.providedRoles -- Backend_provides
Backend.serviceEffectSpecifications -- Backend_query
Backend_provides : OperationProvidedRole { }
Backend_provides.providedInterface -- IBackend

Backend_query : ResourceDemandingSEFF { }
Backend_query.describedService -- query
Backend_query.steps -- Backend_query_start
Backend_query.steps -- Backend_query_cpu
Backend_query.steps -- Backend_query_stop
Backend_query_start : StartAction { }
Backend_query_cpu : InternalAction { entityName = "cpu" }
Backend_query_cpu.resourceDemand -- Backend_query_cpu_demand
Backend_query_cpu_demand : ParametricResourceDemand {
    specification    = "Exp(0.1)"
    requiredResource = ProcessingResourceType.CPU
}
Backend_query_stop : StopAction { }
Backend_query_start.successor -- Backend_query_cpu
Backend_query_cpu.successor -- Backend_query_stop

system : System { entityName = "system" }
system.assemblyContexts -- AssemblyFrontend
system.assemblyContexts -- AssemblyBackend
system.connectors -- delegate
system.connectors -- AssemblyFrontend_to_AssemblyBackend
system.providedRoles -- system_provides
system_provides : OperationProvidedRole { }
system_provides.providedInterface -- IFrontend
AssemblyFrontend : AssemblyContext { entityName = "AssemblyFrontend" }
AssemblyFrontend.encapsulatedComponent -- Frontend
AssemblyBackend : AssemblyContext { entityName = "AssemblyBackend" }
AssemblyBackend.encapsulatedComponent -- Backend
delegate : ProvidedDelegationConnector { }
delegate.assemblyContext -- AssemblyFrontend
delegate.innerProvidedRole -- Frontend_provides
delegate.outerProvidedRole -- system_provides
AssemblyFrontend_to_AssemblyBackend : AssemblyConnector { }
AssemblyFrontend_to_AssemblyBackend.requiringAssemblyContext -- AssemblyFrontend
AssemblyFrontend_to_AssemblyBackend.requiredRole -- Frontend_requires
AssemblyFrontend_to_AssemblyBackend.providingAssemblyContext -- AssemblyBackend
AssemblyFrontend_to_AssemblyBackend.providedRole -- Backend_provides

env : ResourceEnvironment { }
env.resourceContainer -- C1
env.resourceContainer -- C2
env.linkingResources -- lan
C1 : ResourceContainer { entityName = "C1" }
C1.activeResourceSpecifications -- C1_cpu
C1_cpu : ProcessingResourceSpecification {
    schedulingPolicy   = SchedulingPolicy.PROCESSOR_SHARING
    activeResourceType = ProcessingResourceType.CPU
    processingRate     = "1000"
    numberOfReplicas   = 1
}
C2 : ResourceContainer { entityName = "C2" }
C2.activeResourceSpecifications -- C2_cpu
C2_cpu : ProcessingResourceSpecification {
    schedulingPolicy   = SchedulingPolicy.PROCESSOR_SHARING
    activeResourceType = ProcessingResourceType.CPU
    processingRate     = "1000"
    numberOfReplicas   = 1
}
lan : LinkingResource { }
lan.connectedResourceContainers -- C1
lan.connectedResourceContainers -- C2
lan.communicationLinkResourceSpecifications -- lan_spec
lan_spec : CommunicationLinkResourceSpecification {
    communicationLinkResourceType = CommunicationLinkResourceType.LAN
    latency                       = "0"
    throughput                    = "1000000000.0"
    failureProbability            = 0.0
}

allocation : Allocation { entityName = "allocation" }
allocation.system -- system
allocation.targetResourceEnvironment -- env
allocation.allocationContexts -- alloc_Frontend
allocation.allocationContexts -- alloc_Backend
alloc_Frontend : AllocationContext { }
alloc_Frontend.assemblyContext -- AssemblyFrontend
alloc_Frontend.resourceContainer -- C1
alloc_Backend : AllocationContext { }
alloc_Backend.assemblyContext -- AssemblyBackend
alloc_Backend.resourceContainer -- BACKEND_CONTAINER

usage : UsageModel { }
usage.usageScenario -- scenario
scenario : UsageScenario { entityName = "scenario" }
scenario.scenarioBehaviour -- scenario_behaviour
scenario.workload -- scenario_workload
scenario_workload : OpenWorkload { interArrivalTime = "Exp(RATE)" }
scenario_behaviour : ScenarioBehaviour { }
scenario_behaviour.actions -- scenario_start
scenario_behaviour.actions -- scenario_call
scenario_behaviour.actions -- scenario_stop
scenario_start : Start { }
scenario_call : EntryLevelSystemCall { }
scenario_call.providedRole -- system_provides
scenario_call.operationSignature -- handle
scenario_stop : Stop { }
scenario_start.successor -- scenario_call
scenario_call.successor -- scenario_stop

monitors : MonitorRepository { entityName = "monitors" }
monitors.monitors -- rt_scenario
rt_scenario : Monitor { entityName = "rt scenario"  activated = true }
rt_scenario.measuringPoint -- mp_scenario
rt_scenario.measurementSpecifications -- rt_scenario_spec
rt_scenario_spec : MeasurementSpecification {
    metricDescription       = MetricDescription.RESPONSE_TIME
    triggersSelfAdaptations = false
}
rt_scenario_spec.processingType -- rt_scenario_feed
rt_scenario_feed : FeedThrough { }
points : MeasuringPointRepository { }
points.measuringPoints -- mp_scenario
mp_scenario : UsageScenarioMeasuringPoint { }
mp_scenario.usageScenario -- scenario
