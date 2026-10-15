// A chain for the tests of the rule-0 mechanisms (composed calls, bypasses, workflows, read
// stores). Gateway.request (1 ms CPU) calls Frontend.handle (5 ms), which calls Backend.query
// (10 ms, then DbRead.read 2 ms and Other.op 7 ms) and Backend.update (3 ms, then Db.write 2 ms
// and Notifier.notify 4 ms). DbRead.apply takes 1 ms. Time unit: seconds; every CPU does 1000
// units per second, so one unit is 1 ms. Placeholders: RATE, BACKEND_C, NOTIFIER_C, READ_C.

using "../../../palladio-architecture-optimization/metamodel/palladio.mm"

repo : Repository { entityName = "repo" }
repo.interfaces -- IGateway
IGateway : OperationInterface { entityName = "IGateway" }
IGateway.signatures -- IGateway_request
IGateway_request : OperationSignature { entityName = "request" }
repo.interfaces -- IFrontend
IFrontend : OperationInterface { entityName = "IFrontend" }
IFrontend.signatures -- IFrontend_handle
IFrontend_handle : OperationSignature { entityName = "handle" }
repo.interfaces -- IBackend
IBackend : OperationInterface { entityName = "IBackend" }
IBackend.signatures -- IBackend_query
IBackend_query : OperationSignature { entityName = "query" }
IBackend.signatures -- IBackend_update
IBackend_update : OperationSignature { entityName = "update" }
repo.interfaces -- IOther
IOther : OperationInterface { entityName = "IOther" }
IOther.signatures -- IOther_op
IOther_op : OperationSignature { entityName = "op" }
repo.interfaces -- INotifier
INotifier : OperationInterface { entityName = "INotifier" }
INotifier.signatures -- INotifier_notify
INotifier_notify : OperationSignature { entityName = "notify" }
repo.interfaces -- IDb
IDb : OperationInterface { entityName = "IDb" }
IDb.signatures -- IDb_write
IDb_write : OperationSignature { entityName = "write" }
repo.interfaces -- IDbRead
IDbRead : OperationInterface { entityName = "IDbRead" }
IDbRead.signatures -- IDbRead_read
IDbRead_read : OperationSignature { entityName = "read" }
IDbRead.signatures -- IDbRead_apply
IDbRead_apply : OperationSignature { entityName = "apply" }
repo.components -- Gateway
Gateway : BasicComponent { entityName = "Gateway" }
Gateway.providedRoles -- Gateway_provides
Gateway_provides : OperationProvidedRole { }
Gateway_provides.providedInterface -- IGateway
Gateway.requiredRoles -- Gateway_requires_IFrontend
Gateway_requires_IFrontend : OperationRequiredRole { }
Gateway_requires_IFrontend.requiredInterface -- IFrontend
Gateway.serviceEffectSpecifications -- Gateway_request
Gateway_request : ResourceDemandingSEFF { }
Gateway_request.describedService -- IGateway_request
Gateway_request.steps -- Gateway_request_start
Gateway_request_start : StartAction { }
Gateway_request.steps -- Gateway_request_cpu
Gateway_request_cpu : InternalAction { entityName = "cpu" }
Gateway_request_cpu.resourceDemand -- Gateway_request_cpu_demand
Gateway_request_cpu_demand : ParametricResourceDemand { specification = "Exp(1.0)" requiredResource = ProcessingResourceType.CPU }
Gateway_request.steps -- Gateway_request_callHandle
Gateway_request_callHandle : ExternalCallAction { entityName = "callHandle" }
Gateway_request_callHandle.role -- Gateway_requires_IFrontend
Gateway_request_callHandle.calledService -- IFrontend_handle
Gateway_request.steps -- Gateway_request_stop
Gateway_request_stop : StopAction { }
Gateway_request_start.successor -- Gateway_request_cpu
Gateway_request_cpu.successor -- Gateway_request_callHandle
Gateway_request_callHandle.successor -- Gateway_request_stop
repo.components -- Frontend
Frontend : BasicComponent { entityName = "Frontend" }
Frontend.providedRoles -- Frontend_provides
Frontend_provides : OperationProvidedRole { }
Frontend_provides.providedInterface -- IFrontend
Frontend.requiredRoles -- Frontend_requires_IBackend
Frontend_requires_IBackend : OperationRequiredRole { }
Frontend_requires_IBackend.requiredInterface -- IBackend
Frontend.serviceEffectSpecifications -- Frontend_handle
Frontend_handle : ResourceDemandingSEFF { }
Frontend_handle.describedService -- IFrontend_handle
Frontend_handle.steps -- Frontend_handle_start
Frontend_handle_start : StartAction { }
Frontend_handle.steps -- Frontend_handle_cpu
Frontend_handle_cpu : InternalAction { entityName = "cpu" }
Frontend_handle_cpu.resourceDemand -- Frontend_handle_cpu_demand
Frontend_handle_cpu_demand : ParametricResourceDemand { specification = "Exp(0.2)" requiredResource = ProcessingResourceType.CPU }
Frontend_handle.steps -- Frontend_handle_callQuery
Frontend_handle_callQuery : ExternalCallAction { entityName = "callQuery" }
Frontend_handle_callQuery.role -- Frontend_requires_IBackend
Frontend_handle_callQuery.calledService -- IBackend_query
Frontend_handle.steps -- Frontend_handle_callUpdate
Frontend_handle_callUpdate : ExternalCallAction { entityName = "callUpdate" }
Frontend_handle_callUpdate.role -- Frontend_requires_IBackend
Frontend_handle_callUpdate.calledService -- IBackend_update
Frontend_handle.steps -- Frontend_handle_stop
Frontend_handle_stop : StopAction { }
Frontend_handle_start.successor -- Frontend_handle_cpu
Frontend_handle_cpu.successor -- Frontend_handle_callQuery
Frontend_handle_callQuery.successor -- Frontend_handle_callUpdate
Frontend_handle_callUpdate.successor -- Frontend_handle_stop
repo.components -- Backend
Backend : BasicComponent { entityName = "Backend" }
Backend.providedRoles -- Backend_provides
Backend_provides : OperationProvidedRole { }
Backend_provides.providedInterface -- IBackend
Backend.requiredRoles -- Backend_requires_IDbRead
Backend_requires_IDbRead : OperationRequiredRole { }
Backend_requires_IDbRead.requiredInterface -- IDbRead
Backend.requiredRoles -- Backend_requires_IOther
Backend_requires_IOther : OperationRequiredRole { }
Backend_requires_IOther.requiredInterface -- IOther
Backend.requiredRoles -- Backend_requires_IDb
Backend_requires_IDb : OperationRequiredRole { }
Backend_requires_IDb.requiredInterface -- IDb
Backend.requiredRoles -- Backend_requires_INotifier
Backend_requires_INotifier : OperationRequiredRole { }
Backend_requires_INotifier.requiredInterface -- INotifier
Backend.serviceEffectSpecifications -- Backend_query
Backend_query : ResourceDemandingSEFF { }
Backend_query.describedService -- IBackend_query
Backend_query.steps -- Backend_query_start
Backend_query_start : StartAction { }
Backend_query.steps -- Backend_query_cpu
Backend_query_cpu : InternalAction { entityName = "cpu" }
Backend_query_cpu.resourceDemand -- Backend_query_cpu_demand
Backend_query_cpu_demand : ParametricResourceDemand { specification = "Exp(0.1)" requiredResource = ProcessingResourceType.CPU }
Backend_query.steps -- Backend_query_callRead
Backend_query_callRead : ExternalCallAction { entityName = "callRead" }
Backend_query_callRead.role -- Backend_requires_IDbRead
Backend_query_callRead.calledService -- IDbRead_read
Backend_query.steps -- Backend_query_callOther
Backend_query_callOther : ExternalCallAction { entityName = "callOther" }
Backend_query_callOther.role -- Backend_requires_IOther
Backend_query_callOther.calledService -- IOther_op
Backend_query.steps -- Backend_query_stop
Backend_query_stop : StopAction { }
Backend_query_start.successor -- Backend_query_cpu
Backend_query_cpu.successor -- Backend_query_callRead
Backend_query_callRead.successor -- Backend_query_callOther
Backend_query_callOther.successor -- Backend_query_stop
Backend.serviceEffectSpecifications -- Backend_update
Backend_update : ResourceDemandingSEFF { }
Backend_update.describedService -- IBackend_update
Backend_update.steps -- Backend_update_start
Backend_update_start : StartAction { }
Backend_update.steps -- Backend_update_cpu
Backend_update_cpu : InternalAction { entityName = "cpu" }
Backend_update_cpu.resourceDemand -- Backend_update_cpu_demand
Backend_update_cpu_demand : ParametricResourceDemand { specification = "Exp(0.3333333333333333)" requiredResource = ProcessingResourceType.CPU }
Backend_update.steps -- Backend_update_callWrite
Backend_update_callWrite : ExternalCallAction { entityName = "callWrite" }
Backend_update_callWrite.role -- Backend_requires_IDb
Backend_update_callWrite.calledService -- IDb_write
Backend_update.steps -- Backend_update_callNotify
Backend_update_callNotify : ExternalCallAction { entityName = "callNotify" }
Backend_update_callNotify.role -- Backend_requires_INotifier
Backend_update_callNotify.calledService -- INotifier_notify
Backend_update.steps -- Backend_update_stop
Backend_update_stop : StopAction { }
Backend_update_start.successor -- Backend_update_cpu
Backend_update_cpu.successor -- Backend_update_callWrite
Backend_update_callWrite.successor -- Backend_update_callNotify
Backend_update_callNotify.successor -- Backend_update_stop
repo.components -- Other
Other : BasicComponent { entityName = "Other" }
Other.providedRoles -- Other_provides
Other_provides : OperationProvidedRole { }
Other_provides.providedInterface -- IOther
Other.serviceEffectSpecifications -- Other_op
Other_op : ResourceDemandingSEFF { }
Other_op.describedService -- IOther_op
Other_op.steps -- Other_op_start
Other_op_start : StartAction { }
Other_op.steps -- Other_op_cpu
Other_op_cpu : InternalAction { entityName = "cpu" }
Other_op_cpu.resourceDemand -- Other_op_cpu_demand
Other_op_cpu_demand : ParametricResourceDemand { specification = "Exp(0.14285714285714285)" requiredResource = ProcessingResourceType.CPU }
Other_op.steps -- Other_op_stop
Other_op_stop : StopAction { }
Other_op_start.successor -- Other_op_cpu
Other_op_cpu.successor -- Other_op_stop
repo.components -- Notifier
Notifier : BasicComponent { entityName = "Notifier" }
Notifier.providedRoles -- Notifier_provides
Notifier_provides : OperationProvidedRole { }
Notifier_provides.providedInterface -- INotifier
Notifier.serviceEffectSpecifications -- Notifier_notify
Notifier_notify : ResourceDemandingSEFF { }
Notifier_notify.describedService -- INotifier_notify
Notifier_notify.steps -- Notifier_notify_start
Notifier_notify_start : StartAction { }
Notifier_notify.steps -- Notifier_notify_cpu
Notifier_notify_cpu : InternalAction { entityName = "cpu" }
Notifier_notify_cpu.resourceDemand -- Notifier_notify_cpu_demand
Notifier_notify_cpu_demand : ParametricResourceDemand { specification = "Exp(0.25)" requiredResource = ProcessingResourceType.CPU }
Notifier_notify.steps -- Notifier_notify_stop
Notifier_notify_stop : StopAction { }
Notifier_notify_start.successor -- Notifier_notify_cpu
Notifier_notify_cpu.successor -- Notifier_notify_stop
repo.components -- Db
Db : BasicComponent { entityName = "Db" }
Db.providedRoles -- Db_provides
Db_provides : OperationProvidedRole { }
Db_provides.providedInterface -- IDb
Db.serviceEffectSpecifications -- Db_write
Db_write : ResourceDemandingSEFF { }
Db_write.describedService -- IDb_write
Db_write.steps -- Db_write_start
Db_write_start : StartAction { }
Db_write.steps -- Db_write_cpu
Db_write_cpu : InternalAction { entityName = "cpu" }
Db_write_cpu.resourceDemand -- Db_write_cpu_demand
Db_write_cpu_demand : ParametricResourceDemand { specification = "Exp(0.5)" requiredResource = ProcessingResourceType.CPU }
Db_write.steps -- Db_write_stop
Db_write_stop : StopAction { }
Db_write_start.successor -- Db_write_cpu
Db_write_cpu.successor -- Db_write_stop
repo.components -- DbRead
DbRead : BasicComponent { entityName = "DbRead" }
DbRead.providedRoles -- DbRead_provides
DbRead_provides : OperationProvidedRole { }
DbRead_provides.providedInterface -- IDbRead
DbRead.serviceEffectSpecifications -- DbRead_read
DbRead_read : ResourceDemandingSEFF { }
DbRead_read.describedService -- IDbRead_read
DbRead_read.steps -- DbRead_read_start
DbRead_read_start : StartAction { }
DbRead_read.steps -- DbRead_read_cpu
DbRead_read_cpu : InternalAction { entityName = "cpu" }
DbRead_read_cpu.resourceDemand -- DbRead_read_cpu_demand
DbRead_read_cpu_demand : ParametricResourceDemand { specification = "Exp(0.5)" requiredResource = ProcessingResourceType.CPU }
DbRead_read.steps -- DbRead_read_stop
DbRead_read_stop : StopAction { }
DbRead_read_start.successor -- DbRead_read_cpu
DbRead_read_cpu.successor -- DbRead_read_stop
DbRead.serviceEffectSpecifications -- DbRead_apply
DbRead_apply : ResourceDemandingSEFF { }
DbRead_apply.describedService -- IDbRead_apply
DbRead_apply.steps -- DbRead_apply_start
DbRead_apply_start : StartAction { }
DbRead_apply.steps -- DbRead_apply_cpu
DbRead_apply_cpu : InternalAction { entityName = "cpu" }
DbRead_apply_cpu.resourceDemand -- DbRead_apply_cpu_demand
DbRead_apply_cpu_demand : ParametricResourceDemand { specification = "Exp(1.0)" requiredResource = ProcessingResourceType.CPU }
DbRead_apply.steps -- DbRead_apply_stop
DbRead_apply_stop : StopAction { }
DbRead_apply_start.successor -- DbRead_apply_cpu
DbRead_apply_cpu.successor -- DbRead_apply_stop

system : System { entityName = "system" }
system.providedRoles -- system_provides
system_provides : OperationProvidedRole { }
system_provides.providedInterface -- IGateway
system.connectors -- delegate
delegate : ProvidedDelegationConnector { }
delegate.assemblyContext -- Assembly_Gateway
delegate.innerProvidedRole -- Gateway_provides
delegate.outerProvidedRole -- system_provides
system.assemblyContexts -- Assembly_Gateway
Assembly_Gateway : AssemblyContext { entityName = "Assembly_Gateway" }
Assembly_Gateway.encapsulatedComponent -- Gateway
system.assemblyContexts -- Assembly_Frontend
Assembly_Frontend : AssemblyContext { entityName = "Assembly_Frontend" }
Assembly_Frontend.encapsulatedComponent -- Frontend
system.assemblyContexts -- Assembly_Backend
Assembly_Backend : AssemblyContext { entityName = "Assembly_Backend" }
Assembly_Backend.encapsulatedComponent -- Backend
system.assemblyContexts -- Assembly_Other
Assembly_Other : AssemblyContext { entityName = "Assembly_Other" }
Assembly_Other.encapsulatedComponent -- Other
system.assemblyContexts -- Assembly_Notifier
Assembly_Notifier : AssemblyContext { entityName = "Assembly_Notifier" }
Assembly_Notifier.encapsulatedComponent -- Notifier
system.assemblyContexts -- Assembly_Db
Assembly_Db : AssemblyContext { entityName = "Assembly_Db" }
Assembly_Db.encapsulatedComponent -- Db
system.assemblyContexts -- Assembly_DbRead
Assembly_DbRead : AssemblyContext { entityName = "Assembly_DbRead" }
Assembly_DbRead.encapsulatedComponent -- DbRead
system.connectors -- Assembly_Gateway_to_Assembly_Frontend
Assembly_Gateway_to_Assembly_Frontend : AssemblyConnector { }
Assembly_Gateway_to_Assembly_Frontend.requiringAssemblyContext -- Assembly_Gateway
Assembly_Gateway_to_Assembly_Frontend.requiredRole -- Gateway_requires_IFrontend
Assembly_Gateway_to_Assembly_Frontend.providingAssemblyContext -- Assembly_Frontend
Assembly_Gateway_to_Assembly_Frontend.providedRole -- Frontend_provides
system.connectors -- Assembly_Frontend_to_Assembly_Backend
Assembly_Frontend_to_Assembly_Backend : AssemblyConnector { }
Assembly_Frontend_to_Assembly_Backend.requiringAssemblyContext -- Assembly_Frontend
Assembly_Frontend_to_Assembly_Backend.requiredRole -- Frontend_requires_IBackend
Assembly_Frontend_to_Assembly_Backend.providingAssemblyContext -- Assembly_Backend
Assembly_Frontend_to_Assembly_Backend.providedRole -- Backend_provides
system.connectors -- Assembly_Backend_to_Assembly_DbRead
Assembly_Backend_to_Assembly_DbRead : AssemblyConnector { }
Assembly_Backend_to_Assembly_DbRead.requiringAssemblyContext -- Assembly_Backend
Assembly_Backend_to_Assembly_DbRead.requiredRole -- Backend_requires_IDbRead
Assembly_Backend_to_Assembly_DbRead.providingAssemblyContext -- Assembly_DbRead
Assembly_Backend_to_Assembly_DbRead.providedRole -- DbRead_provides
system.connectors -- Assembly_Backend_to_Assembly_Other
Assembly_Backend_to_Assembly_Other : AssemblyConnector { }
Assembly_Backend_to_Assembly_Other.requiringAssemblyContext -- Assembly_Backend
Assembly_Backend_to_Assembly_Other.requiredRole -- Backend_requires_IOther
Assembly_Backend_to_Assembly_Other.providingAssemblyContext -- Assembly_Other
Assembly_Backend_to_Assembly_Other.providedRole -- Other_provides
system.connectors -- Assembly_Backend_to_Assembly_Db
Assembly_Backend_to_Assembly_Db : AssemblyConnector { }
Assembly_Backend_to_Assembly_Db.requiringAssemblyContext -- Assembly_Backend
Assembly_Backend_to_Assembly_Db.requiredRole -- Backend_requires_IDb
Assembly_Backend_to_Assembly_Db.providingAssemblyContext -- Assembly_Db
Assembly_Backend_to_Assembly_Db.providedRole -- Db_provides
system.connectors -- Assembly_Backend_to_Assembly_Notifier
Assembly_Backend_to_Assembly_Notifier : AssemblyConnector { }
Assembly_Backend_to_Assembly_Notifier.requiringAssemblyContext -- Assembly_Backend
Assembly_Backend_to_Assembly_Notifier.requiredRole -- Backend_requires_INotifier
Assembly_Backend_to_Assembly_Notifier.providingAssemblyContext -- Assembly_Notifier
Assembly_Backend_to_Assembly_Notifier.providedRole -- Notifier_provides

env : ResourceEnvironment { }
env.linkingResources -- lan
lan : LinkingResource { }
env.resourceContainer -- C0
C0 : ResourceContainer { entityName = "C0" }
C0.activeResourceSpecifications -- C0_cpu
C0_cpu : ProcessingResourceSpecification { schedulingPolicy = SchedulingPolicy.PROCESSOR_SHARING activeResourceType = ProcessingResourceType.CPU processingRate = "1000" numberOfReplicas = 1 }
lan.connectedResourceContainers -- C0
env.resourceContainer -- C1
C1 : ResourceContainer { entityName = "C1" }
C1.activeResourceSpecifications -- C1_cpu
C1_cpu : ProcessingResourceSpecification { schedulingPolicy = SchedulingPolicy.PROCESSOR_SHARING activeResourceType = ProcessingResourceType.CPU processingRate = "1000" numberOfReplicas = 1 }
lan.connectedResourceContainers -- C1
env.resourceContainer -- C2
C2 : ResourceContainer { entityName = "C2" }
C2.activeResourceSpecifications -- C2_cpu
C2_cpu : ProcessingResourceSpecification { schedulingPolicy = SchedulingPolicy.PROCESSOR_SHARING activeResourceType = ProcessingResourceType.CPU processingRate = "1000" numberOfReplicas = 1 }
lan.connectedResourceContainers -- C2
env.resourceContainer -- C3
C3 : ResourceContainer { entityName = "C3" }
C3.activeResourceSpecifications -- C3_cpu
C3_cpu : ProcessingResourceSpecification { schedulingPolicy = SchedulingPolicy.PROCESSOR_SHARING activeResourceType = ProcessingResourceType.CPU processingRate = "1000" numberOfReplicas = 1 }
lan.connectedResourceContainers -- C3
env.resourceContainer -- C4
C4 : ResourceContainer { entityName = "C4" }
C4.activeResourceSpecifications -- C4_cpu
C4_cpu : ProcessingResourceSpecification { schedulingPolicy = SchedulingPolicy.PROCESSOR_SHARING activeResourceType = ProcessingResourceType.CPU processingRate = "1000" numberOfReplicas = 1 }
lan.connectedResourceContainers -- C4
env.resourceContainer -- C5
C5 : ResourceContainer { entityName = "C5" }
C5.activeResourceSpecifications -- C5_cpu
C5_cpu : ProcessingResourceSpecification { schedulingPolicy = SchedulingPolicy.PROCESSOR_SHARING activeResourceType = ProcessingResourceType.CPU processingRate = "1000" numberOfReplicas = 1 }
lan.connectedResourceContainers -- C5
env.resourceContainer -- C6
C6 : ResourceContainer { entityName = "C6" }
C6.activeResourceSpecifications -- C6_cpu
C6_cpu : ProcessingResourceSpecification { schedulingPolicy = SchedulingPolicy.PROCESSOR_SHARING activeResourceType = ProcessingResourceType.CPU processingRate = "1000" numberOfReplicas = 1 }
lan.connectedResourceContainers -- C6
lan.communicationLinkResourceSpecifications -- lan_spec
lan_spec : CommunicationLinkResourceSpecification { communicationLinkResourceType = CommunicationLinkResourceType.LAN latency = "0" throughput = "1000000000.0" failureProbability = 0.0 }

allocation : Allocation { entityName = "allocation" }
allocation.system -- system
allocation.targetResourceEnvironment -- env
allocation.allocationContexts -- alloc_Gateway
alloc_Gateway : AllocationContext { }
alloc_Gateway.assemblyContext -- Assembly_Gateway
alloc_Gateway.resourceContainer -- C0
allocation.allocationContexts -- alloc_Frontend
alloc_Frontend : AllocationContext { }
alloc_Frontend.assemblyContext -- Assembly_Frontend
alloc_Frontend.resourceContainer -- C1
allocation.allocationContexts -- alloc_Backend
alloc_Backend : AllocationContext { }
alloc_Backend.assemblyContext -- Assembly_Backend
alloc_Backend.resourceContainer -- BACKEND_C
allocation.allocationContexts -- alloc_Other
alloc_Other : AllocationContext { }
alloc_Other.assemblyContext -- Assembly_Other
alloc_Other.resourceContainer -- BACKEND_C
allocation.allocationContexts -- alloc_Notifier
alloc_Notifier : AllocationContext { }
alloc_Notifier.assemblyContext -- Assembly_Notifier
alloc_Notifier.resourceContainer -- NOTIFIER_C
allocation.allocationContexts -- alloc_Db
alloc_Db : AllocationContext { }
alloc_Db.assemblyContext -- Assembly_Db
alloc_Db.resourceContainer -- C3
allocation.allocationContexts -- alloc_DbRead
alloc_DbRead : AllocationContext { }
alloc_DbRead.assemblyContext -- Assembly_DbRead
alloc_DbRead.resourceContainer -- READ_C

usage : UsageModel { }
usage.usageScenario -- scenario
scenario : UsageScenario { entityName = "scenario" }
scenario.scenarioBehaviour -- scenario_behaviour
scenario.workload -- scenario_workload
scenario_workload : OpenWorkload { interArrivalTime = "Exp(RATE)" }
scenario_behaviour : ScenarioBehaviour { }
scenario_behaviour.actions -- scenario_start
scenario_start : Start { }
scenario_behaviour.actions -- scenario_call
scenario_call : EntryLevelSystemCall { }
scenario_behaviour.actions -- scenario_stop
scenario_stop : Stop { }
scenario_call.providedRole -- system_provides
scenario_call.operationSignature -- IGateway_request
scenario_start.successor -- scenario_call
scenario_call.successor -- scenario_stop
monitors : MonitorRepository { entityName = "monitors" }
monitors.monitors -- rt_scenario
rt_scenario : Monitor { entityName = "rt scenario" activated = true }
rt_scenario.measuringPoint -- mp_scenario
rt_scenario.measurementSpecifications -- rt_scenario_spec
rt_scenario_spec : MeasurementSpecification { metricDescription = MetricDescription.RESPONSE_TIME triggersSelfAdaptations = false }
rt_scenario_spec.processingType -- rt_scenario_feed
rt_scenario_feed : FeedThrough { }
points : MeasuringPointRepository { }
points.measuringPoints -- mp_scenario
mp_scenario : UsageScenarioMeasuringPoint { }
mp_scenario.usageScenario -- scenario
