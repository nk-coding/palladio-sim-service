// The Palladio Component Model (PCM 5.2), as simulated by SimuLizar 5.2.2.
//
// Generated from the PCM Ecore metamodel by `palladio-sim-service gen-metamodel`: the
// subset of classes and features the simulator reads. Mapping rules:
// - classes keep their PCM names; features drop their EMF suffix
//   (`components__Repository` -> `components`);
// - an instance's name is its PCM id;
// - stochastic expressions (PCMRandomVariable) are strings holding the StoEx, e.g. "Exp(2.0)";
// - variable references are dot-separated names, e.g. "items.NUMBER_OF_ELEMENTS";
// - references into Palladio's library models (resource types, scheduling policies,
//   resource interfaces, metrics) are enums.
//
// Every root (Repository, System, ResourceEnvironment, Allocation, UsageModel,
// MonitorRepository, MeasuringPointRepository) becomes one PCM file for the simulator.
//
// The component types, interfaces, signatures, roles and data types live in
// ./pcm/repository.mm, which this file imports; everything else lives here.
//
// BasicComponent, CompositeComponent, ImplementationComponentType and PassiveResource are
// repository classes, but live here: the metamodel language declares an association end with
// its class, and parameter usages (VariableUsage) are contained by component implementations,
// SEFF actions, assembly contexts and usage model calls alike. The views at the end show the
// metamodel by package group, wherever a class lives.

import "./pcm/repository.mm"

// ---------------------------------------------------------------------------------------
// Enums

enum ComponentType {
    BUSINESS_COMPONENT
    INFRASTRUCTURE_COMPONENT
}

enum VariableCharacterisationType {
    STRUCTURE
    NUMBER_OF_ELEMENTS
    VALUE
    BYTESIZE
    TYPE
}

enum ResourceSignature { // elements of Palladio's library models
    PROCESS
    READ
    WRITE
}

enum ProcessingResourceType { // elements of Palladio's library models
    CPU
    HDD
    DELAY
}

enum SchedulingPolicy { // elements of Palladio's library models
    PROCESSOR_SHARING
    FIRST_COME_FIRST_SERVE
    DELAY
}

enum CommunicationLinkResourceType { // elements of Palladio's library models
    LAN
}

enum MetricDescription { // elements of Palladio's library models
    POINT_IN_TIME
    RESPONSE_TIME
    RESPONSE_TIME_TUPLE
    HOLDING_TIME
    HOLDING_TIME_TUPLE
    WAITING_TIME
    WAITING_TIME_TUPLE
    RESOURCE_DEMAND
    RESOURCE_DEMAND_TUPLE
    STATE_OF_ACTIVE_RESOURCE
    STATE_OF_ACTIVE_RESOURCE_TUPLE
    OVERALL_STATE_OF_ACTIVE_RESOURCE
    STATE_OF_ACTIVE_RESOURCE_OVER_TIME
    STATE_OF_PASSIVE_RESOURCE
    STATE_OF_PASSIVE_RESOURCE_TUPLE
    OVERALL_STATE_OF_PASSIVE_RESOURCE
    STATE_OF_PASSIVE_RESOURCE_OVER_TIME
    EXECUTION_RESULT
    EXECUTION_RESULT_TUPLE
    SCALABILITY_RANGE
    SLOPE
    SCALABILITY_SPEED
    USER_CAPACITY
    USER_CAPACITY_TUPLE
    INTERARRIVAL_TIME_CAPACITY
    INTERARRIVAL_TIME_CAPACITY_TUPLE
    NUMBER_OF_SLO_VIOLATIONS
    NUMBER_OF_SLO_VIOLATIONS_OVER_TIME
    MEAN_TIME_TO_QUALITY_REPAIR
    RESOURCE_PROVISIONING_EFFICIENCY
    MARGINAL_COST
    UTILIZATION_OF_ACTIVE_RESOURCE
    UTILIZATION_OF_ACTIVE_RESOURCE_TUPLE
    POWER_CONSUMPTION
    POWER_CONSUMPTION_TUPLE
    ENERGY_CONSUMPTION
    CUMULATIVE_ENERGY_CONSUMPTION_TUPLE
    NUMBER_OF_RESOURCE_CONTAINERS
    NUMBER_OF_RESOURCE_CONTAINERS_OVER_TIME
    COST_OVER_TIME
    AGGREGATED_COST_OVER_TIME
    COST_PER_RESOURCE_CONTAINER
    COST_OF_RESOURCE_CONTAINERS
    RECONFIGURATION_TIME
    RECONFIGURATION_TIME_TUPLE
    OPTIMISATION_TIME
    OPTIMISATION_TIME_TUPLE
    EXTERNAL_EVENT_TIME
    NUMBER_OF_CONCURRENTLY_EXECUTING_INVOCATIONS
    NUMBER_OF_CONCURRENTLY_EXECUTING_INVOCATIONS_TUPLE
    REQUEST_ARRIVAL_RATE
    REQUEST_ARRIVAL_RATE_TUPLE
    RESPONSE_ARRIVAL_RATE
    RESPONSE_ARRIVAL_RATE_TUPLE
    READ_DATA_RATE
    READ_DATA_RATE_TUPLE
    WRITE_DATA_RATE
    WRITE_DATA_RATE_TUPLE
    READ_UTILIZATION
    WRITE_UTILIZATION
    READ_UTILIZATION_OVER_TIME
    WRITE_UTILIZATION_OVER_TIME
    EXECUTION_RESULT_TYPE
    EXECUTION_RESULT_TYPE_OVER_TIME
}

// ---------------------------------------------------------------------------------------
// Repository: components, interfaces, signatures, data types

class BasicComponent extends ImplementationComponentType {}

class CompositeComponent extends ComposedStructure, ImplementationComponentType {}

abstract class ImplementationComponentType extends RepositoryComponent {
    componentType: ComponentType[?]
}

class PassiveResource extends NamedElement {
    capacity: string[?]
}

class SubSystem extends ComposedStructure, RepositoryComponent {}

BasicComponent.serviceEffectSpecifications[*] *--> ServiceEffectSpecification.basicComponent[?]
BasicComponent.passiveResource[*] *--> PassiveResource.basicComponent[?]
ImplementationComponentType.componentParameterUsage[*] *--> VariableUsage.componentParameterUsageOwner[?]

// ---------------------------------------------------------------------------------------
// Behaviour (SEFFs): actions, resource demands, calls

abstract class AbstractAction extends NamedElement {}

abstract class AbstractBranchTransition extends NamedElement {}

abstract class AbstractInternalControlFlowAction extends AbstractAction {}

abstract class AbstractLoopAction extends AbstractInternalControlFlowAction {}

class AcquireAction extends AbstractInternalControlFlowAction {
    timeout: boolean[?]
    timeoutValue: double[?]
}

class BranchAction extends AbstractInternalControlFlowAction {}

abstract class CallAction extends NamedElement {}

class CallReturnAction extends CallAction {}

class CollectionIteratorAction extends AbstractLoopAction {}

class EmitEventAction extends AbstractAction, CallAction {}

class ExternalCallAction extends AbstractAction, CallReturnAction {
    retryCount: int[?]
}

class ForkAction extends AbstractInternalControlFlowAction {}

class ForkedBehaviour extends ResourceDemandingBehaviour {}

class GuardedBranchTransition extends AbstractBranchTransition {
    branchCondition: string[?]
}

class InfrastructureCall extends CallAction {
    numberOfCalls: string[?]
}

class InternalAction extends AbstractInternalControlFlowAction {}

class InternalCallAction extends AbstractInternalControlFlowAction, CallAction {}

class LoopAction extends AbstractLoopAction {
    iterationCount: string[?]
}

class ParametricResourceDemand {
    specification: string[?]
    requiredResource: ProcessingResourceType[?]
}

class ProbabilisticBranchTransition extends AbstractBranchTransition {
    branchProbability: double[?]
}

class RecoveryAction extends AbstractInternalControlFlowAction {}

class RecoveryActionBehaviour extends NamedElement, ResourceDemandingBehaviour {}

class ReleaseAction extends AbstractInternalControlFlowAction {}

class ResourceCall extends CallAction {
    signature: ResourceSignature[?]
    numberOfCalls: string[?]
}

class ResourceDemandingBehaviour {}

class ResourceDemandingInternalBehaviour extends ResourceDemandingBehaviour {}

class ResourceDemandingSEFF extends ResourceDemandingBehaviour, ServiceEffectSpecification {}

abstract class ServiceEffectSpecification {
    seffTypeID: string[?]
}

class SetVariableAction extends AbstractInternalControlFlowAction {}

class StartAction extends AbstractInternalControlFlowAction {}

class StopAction extends AbstractInternalControlFlowAction {}

class SynchronisationPoint {}

AbstractInternalControlFlowAction.resourceDemand[*] *--> ParametricResourceDemand.action[?]
AbstractInternalControlFlowAction.infrastructureCall[*] *--> InfrastructureCall.action[?]
AbstractInternalControlFlowAction.resourceCall[*] *--> ResourceCall.action[?]
AbstractAction.predecessor[?] <--> AbstractAction.successor[?]
ResourceDemandingBehaviour.steps[*] *--> AbstractAction.resourceDemandingBehaviour[?]
AbstractLoopAction.bodyBehaviour[?] *--> ResourceDemandingBehaviour.abstractLoopAction[?]
AbstractBranchTransition.branchBehaviour[?] *--> ResourceDemandingBehaviour.abstractBranchTransition[?]
BranchAction.branches[*] *--> AbstractBranchTransition.branchAction[?]
CallAction.inputVariableUsages[*] *--> VariableUsage.callAction[?]
ServiceEffectSpecification.describedService[?] --> Signature
ResourceDemandingSEFF.resourceDemandingInternalBehaviours[*] *--> ResourceDemandingInternalBehaviour.resourceDemandingSEFF[?]
ReleaseAction.passiveResource[?] --> PassiveResource
ForkAction.asynchronousForkedBehaviours[*] *--> ForkedBehaviour.forkAction[?]
ForkAction.synchronisingBehaviours[?] *--> SynchronisationPoint.forkAction[?]
SynchronisationPoint.outputParameterUsage[*] *--> VariableUsage.synchronisationPoint[?]
SynchronisationPoint.synchronousForkedBehaviours[*] *--> ForkedBehaviour.synchronisationPoint[?]
ExternalCallAction.calledService[?] --> OperationSignature
ExternalCallAction.role[?] --> OperationRequiredRole
CallReturnAction.returnVariableUsage[*] *--> VariableUsage.callReturnAction[?]
AcquireAction.passiveresource[?] --> PassiveResource
CollectionIteratorAction.parameter[?] --> Parameter
SetVariableAction.localVariableUsages[*] *--> VariableUsage.setVariableAction[?]
InternalCallAction.calledResourceDemandingInternalBehaviour[?] --> ResourceDemandingInternalBehaviour
EmitEventAction.eventType[?] --> EventType
EmitEventAction.sourceRole[?] --> SourceRole
InfrastructureCall.signature[?] --> InfrastructureSignature
InfrastructureCall.requiredRole[?] --> InfrastructureRequiredRole
ResourceCall.resourceRequiredRole[?] --> ResourceRequiredRole
RecoveryAction.primaryBehaviour[?] --> RecoveryActionBehaviour
RecoveryAction.recoveryActionBehaviours[*] *--> RecoveryActionBehaviour.recoveryAction[?]

// ---------------------------------------------------------------------------------------
// Parameters: variable usages and characterisations

class VariableCharacterisation {
    type: VariableCharacterisationType[?]
    specification: string[?]
}

class VariableUsage {
    namedReference: string[?]
}

VariableUsage.variableCharacterisation[*] *--> VariableCharacterisation.variableUsage[?]

// ---------------------------------------------------------------------------------------
// Composition and system: assembly contexts and connectors

class AssemblyConnector extends Connector {}

class AssemblyContext extends NamedElement {}

class AssemblyEventConnector extends Connector {}

class AssemblyInfrastructureConnector extends Connector {}

abstract class ComposedStructure extends NamedElement {}

abstract class Connector extends NamedElement {}

class EventChannelSinkConnector extends Connector {}

class EventChannelSourceConnector extends Connector {}

class ProvidedDelegationConnector extends Connector {}

class ProvidedInfrastructureDelegationConnector extends Connector {}

class RequiredDelegationConnector extends Connector {}

class RequiredInfrastructureDelegationConnector extends Connector {}

class RequiredResourceDelegationConnector extends Connector {}

class SinkDelegationConnector extends Connector {}

class SourceDelegationConnector extends Connector {}

class System extends InterfaceProvidingEntity, InterfaceRequiringEntity, ComposedStructure {}

ComposedStructure.assemblyContexts[*] *--> AssemblyContext.parentStructure[?]
ComposedStructure.connectors[*] *--> Connector.parentStructure[?]
ProvidedDelegationConnector.innerProvidedRole[?] --> OperationProvidedRole
ProvidedDelegationConnector.outerProvidedRole[?] --> OperationProvidedRole
ProvidedDelegationConnector.assemblyContext[?] --> AssemblyContext
RequiredDelegationConnector.innerRequiredRole[?] --> OperationRequiredRole
RequiredDelegationConnector.outerRequiredRole[?] --> OperationRequiredRole
RequiredDelegationConnector.assemblyContext[?] --> AssemblyContext
AssemblyConnector.requiringAssemblyContext[?] --> AssemblyContext
AssemblyConnector.providingAssemblyContext[?] --> AssemblyContext
AssemblyConnector.providedRole[?] --> OperationProvidedRole
AssemblyConnector.requiredRole[?] --> OperationRequiredRole
AssemblyInfrastructureConnector.providedRole[?] --> InfrastructureProvidedRole
AssemblyInfrastructureConnector.requiredRole[?] --> InfrastructureRequiredRole
AssemblyInfrastructureConnector.providingAssemblyContext[?] --> AssemblyContext
AssemblyInfrastructureConnector.requiringAssemblyContext[?] --> AssemblyContext
ProvidedInfrastructureDelegationConnector.innerProvidedRole[?] --> InfrastructureProvidedRole
ProvidedInfrastructureDelegationConnector.outerProvidedRole[?] --> InfrastructureProvidedRole
ProvidedInfrastructureDelegationConnector.assemblyContext[?] --> AssemblyContext
RequiredInfrastructureDelegationConnector.innerRequiredRole[?] --> InfrastructureRequiredRole
RequiredInfrastructureDelegationConnector.outerRequiredRole[?] --> InfrastructureRequiredRole
RequiredInfrastructureDelegationConnector.assemblyContext[?] --> AssemblyContext
RequiredResourceDelegationConnector.assemblyContext[?] --> AssemblyContext
RequiredResourceDelegationConnector.innerRequiredRole[?] --> ResourceRequiredRole
RequiredResourceDelegationConnector.outerRequiredRole[?] --> ResourceRequiredRole
AssemblyContext.encapsulatedComponent[?] --> RepositoryComponent
AssemblyContext.configParameterUsages[*] *--> VariableUsage.assemblyContext[?]

// ---------------------------------------------------------------------------------------
// Resource environment: containers, processing and linking resources

class CommunicationLinkResourceSpecification {
    failureProbability: double[?]
    communicationLinkResourceType: CommunicationLinkResourceType[?]
    latency: string[?]
    throughput: string[?]
}

class HDDProcessingResourceSpecification extends ProcessingResourceSpecification {
    writeProcessingRate: string[?]
    readProcessingRate: string[?]
}

class LinkingResource extends NamedElement {}

class ProcessingResourceSpecification {
    MTTR: double[?]
    MTTF: double[?]
    requiredByContainer: boolean[?]
    schedulingPolicy: SchedulingPolicy[?]
    activeResourceType: ProcessingResourceType[?]
    processingRate: string[?]
    numberOfReplicas: int[?]
}

class ResourceContainer extends NamedElement {}

class ResourceEnvironment extends NamedElement {}

ResourceEnvironment.linkingResources[*] *--> LinkingResource.resourceEnvironment[?]
ResourceEnvironment.resourceContainer[*] *--> ResourceContainer.resourceEnvironment[?]
LinkingResource.connectedResourceContainers[*] --> ResourceContainer
LinkingResource.communicationLinkResourceSpecifications[?] *--> CommunicationLinkResourceSpecification.linkingResource[?]
ResourceContainer.activeResourceSpecifications[*] *--> ProcessingResourceSpecification.resourceContainer[?]
ResourceContainer.nestedResourceContainers[*] *--> ResourceContainer.parentResourceContainer[?]

// ---------------------------------------------------------------------------------------
// Allocation

class Allocation extends NamedElement {}

class AllocationContext extends NamedElement {}

AllocationContext.resourceContainer[?] --> ResourceContainer
AllocationContext.assemblyContext[?] --> AssemblyContext
Allocation.targetResourceEnvironment[?] --> ResourceEnvironment
Allocation.system[?] --> System
Allocation.allocationContexts[*] *--> AllocationContext.allocation[?]

// ---------------------------------------------------------------------------------------
// Usage model: scenarios, workloads, user behaviour

abstract class AbstractUserAction extends NamedElement {}

class Branch extends AbstractUserAction {}

class BranchTransition {
    branchProbability: double[?]
}

class ClosedWorkload extends Workload {
    population: int[?]
    thinkTime: string[?]
}

class Delay extends AbstractUserAction {
    timeSpecification: string[?]
}

class EntryLevelSystemCall extends AbstractUserAction {
    priority: int[?]
}

class Loop extends AbstractUserAction {
    loopIteration: string[?]
}

class OpenWorkload extends Workload {
    interArrivalTime: string[?]
}

class ScenarioBehaviour extends NamedElement {}

class Start extends AbstractUserAction {}

class Stop extends AbstractUserAction {}

class UsageModel {}

class UsageScenario extends NamedElement {}

class UserData {}

abstract class Workload {}

UsageScenario.scenarioBehaviour[?] *--> ScenarioBehaviour.usageScenario[?]
UsageScenario.workload[?] *--> Workload.usageScenario[?]
UserData.assemblyContext[?] --> AssemblyContext
UserData.userDataParameterUsages[*] *--> VariableUsage.userData[?]
UsageModel.usageScenario[*] *--> UsageScenario.usageModel[?]
UsageModel.userData[*] *--> UserData.usageModel[?]
EntryLevelSystemCall.providedRole[?] --> OperationProvidedRole
EntryLevelSystemCall.operationSignature[?] --> OperationSignature
EntryLevelSystemCall.outputParameterUsages[*] *--> VariableUsage.entryLevelSystemCall_OutputParameterUsage[?]
EntryLevelSystemCall.inputParameterUsages[*] *--> VariableUsage.entryLevelSystemCall_InputParameterUsage[?]
AbstractUserAction.successor[?] <--> AbstractUserAction.predecessor[?]
ScenarioBehaviour.actions[*] *--> AbstractUserAction.scenarioBehaviour[?]
BranchTransition.branchedBehaviour[?] *--> ScenarioBehaviour.branchTransition[?]
Branch.branchTransitions[*] *--> BranchTransition.branch[?]
Loop.bodyBehaviour[?] *--> ScenarioBehaviour.loop[?]

// ---------------------------------------------------------------------------------------
// Monitoring: monitors, measurement specifications and measuring points

class ActiveResourceMeasuringPoint extends MeasuringPoint, ActiveResourceReference {}

abstract class ActiveResourceReference {
    replicaID: int[?]
}

abstract class Aggregation extends ProcessingType {}

class ArithmeticMean extends StatisticalCharacterization {}

class AssemblyOperationMeasuringPoint extends MeasuringPoint, OperationReference, AssemblyReference {}

class AssemblyPassiveResourceMeasuringPoint extends MeasuringPoint, AssemblyReference, PassiveResourceReference {}

abstract class AssemblyReference {}

class EntryLevelSystemCallMeasuringPoint extends MeasuringPoint, EntryLevelSystemCallReference {}

abstract class EntryLevelSystemCallReference {}

class ExternalCallActionMeasuringPoint extends MeasuringPoint, ExternalCallActionReference {}

abstract class ExternalCallActionReference {}

class FeedThrough extends ProcessingType {}

class FixedSizeAggregation extends MeasurementDrivenAggregation {
    numberOfMeasurements: int[?]
}

class GeometricMean extends StatisticalCharacterization {}

class HarmonicMean extends StatisticalCharacterization {}

class LinkingResourceMeasuringPoint extends MeasuringPoint, LinkingResourceReference {}

abstract class LinkingResourceReference {}

abstract class MeasurementDrivenAggregation extends Aggregation {
    frequency: int[?]
}

class MeasurementSpecification {
    metricDescription: MetricDescription[?]
    triggersSelfAdaptations: boolean[?]
}

abstract class MeasuringPoint {
    stringRepresentation: string[?]
    resourceURIRepresentation: string[?]
}

class MeasuringPointRepository {}

class Median extends StatisticalCharacterization {}

class Monitor extends NamedElement {
    activated: boolean[?]
}

class MonitorRepository extends NamedElement {}

abstract class OperationReference {}

abstract class PassiveResourceReference {}

abstract class ProcessingType {}

class ReconfigurationMeasuringPoint extends ResourceURIMeasuringPoint {}

class ResourceContainerMeasuringPoint extends MeasuringPoint, ResourceContainerReference {}

abstract class ResourceContainerReference {}

class ResourceEnvironmentMeasuringPoint extends MeasuringPoint, ResourceEnvironmentReference {}

abstract class ResourceEnvironmentReference {}

class ResourceURIMeasuringPoint extends StringMeasuringPoint {
    resourceURI: string[?]
}

abstract class StatisticalCharacterization {}

class StringMeasuringPoint extends MeasuringPoint {
    measuringPoint: string[?]
}

class SubSystemOperationMeasuringPoint extends MeasuringPoint, OperationReference, SubSystemReference {}

abstract class SubSystemReference {}

class SystemOperationMeasuringPoint extends MeasuringPoint, OperationReference, SystemReference {}

abstract class SystemReference {}

class TimeDriven extends ProcessingType {
    windowLength: double[?]
    windowIncrement: double[?]
}

class TimeDrivenAggregation extends Aggregation, TimeDriven {}

class UsageScenarioMeasuringPoint extends MeasuringPoint, UsageScenarioReference {}

abstract class UsageScenarioReference {}

class VariableSizeAggregation extends MeasurementDrivenAggregation {
    retrospectionLength: double[?]
}

MeasuringPointRepository.measuringPoints[*] *--> MeasuringPoint.measuringPointRepository[?]
MonitorRepository.monitors[*] *--> Monitor.monitorRepository[?]
Monitor.measurementSpecifications[*] *--> MeasurementSpecification.monitor[?]
Monitor.measuringPoint[?] --> MeasuringPoint
MeasurementSpecification.processingType[?] *--> ProcessingType.measurementSpecification[?]
Aggregation.statisticalCharacterization[?] *--> StatisticalCharacterization.statisticalCharacterizationOwner[?]
OperationReference.role[?] --> Role
OperationReference.operationSignature[?] --> OperationSignature
AssemblyReference.assembly[?] --> AssemblyContext
SystemReference.system[?] --> System
PassiveResourceReference.passiveResource[?] --> PassiveResource
ActiveResourceReference.activeResource[?] --> ProcessingResourceSpecification
LinkingResourceReference.linkingResource[?] --> LinkingResource
SubSystemReference.subsystem[?] --> SubSystem
UsageScenarioReference.usageScenario[?] --> UsageScenario
ExternalCallActionReference.externalCall[?] --> ExternalCallAction
EntryLevelSystemCallReference.entryLevelSystemCall[?] --> EntryLevelSystemCall
ResourceEnvironmentReference.resourceEnvironment[?] --> ResourceEnvironment
ResourceContainerReference.resourceContainer[?] --> ResourceContainer

// ---------------------------------------------------------------------------------------
// Views: the metamodel by package group

view Core {
    InterfaceProvidingEntity
    InterfaceRequiringEntity
    NamedElement
    ResourceInterfaceRequiringEntity
    ResourceProvidedRole
    ResourceRequiredRole
}

view Repository {
    BasicComponent
    CollectionDataType
    CompleteComponentType
    CompositeComponent
    CompositeDataType
    DataType
    EventGroup
    EventType
    ImplementationComponentType
    InfrastructureInterface
    InfrastructureProvidedRole
    InfrastructureRequiredRole
    InfrastructureSignature
    InnerDeclaration
    Interface
    OperationInterface
    OperationProvidedRole
    OperationRequiredRole
    OperationSignature
    Parameter
    PassiveResource
    PrimitiveDataType
    ProvidedRole
    ProvidesComponentType
    Repository
    RepositoryComponent
    RequiredRole
    Role
    Signature
    SinkRole
    SourceRole
    SubSystem
}

view Behaviour {
    AbstractAction
    AbstractBranchTransition
    AbstractInternalControlFlowAction
    AbstractLoopAction
    AcquireAction
    BranchAction
    CallAction
    CallReturnAction
    CollectionIteratorAction
    EmitEventAction
    ExternalCallAction
    ForkAction
    ForkedBehaviour
    GuardedBranchTransition
    InfrastructureCall
    InternalAction
    InternalCallAction
    LoopAction
    ParametricResourceDemand
    ProbabilisticBranchTransition
    RecoveryAction
    RecoveryActionBehaviour
    ReleaseAction
    ResourceCall
    ResourceDemandingBehaviour
    ResourceDemandingInternalBehaviour
    ResourceDemandingSEFF
    ServiceEffectSpecification
    SetVariableAction
    StartAction
    StopAction
    SynchronisationPoint
}

view Parameters {
    VariableCharacterisation
    VariableUsage
}

view Composition {
    AssemblyConnector
    AssemblyContext
    AssemblyEventConnector
    AssemblyInfrastructureConnector
    ComposedStructure
    Connector
    EventChannelSinkConnector
    EventChannelSourceConnector
    ProvidedDelegationConnector
    ProvidedInfrastructureDelegationConnector
    RequiredDelegationConnector
    RequiredInfrastructureDelegationConnector
    RequiredResourceDelegationConnector
    SinkDelegationConnector
    SourceDelegationConnector
    System
}

view ResourceEnvironment {
    CommunicationLinkResourceSpecification
    HDDProcessingResourceSpecification
    LinkingResource
    ProcessingResourceSpecification
    ResourceContainer
    ResourceEnvironment
}

view Allocation {
    Allocation
    AllocationContext
}

view UsageModel {
    AbstractUserAction
    Branch
    BranchTransition
    ClosedWorkload
    Delay
    EntryLevelSystemCall
    Loop
    OpenWorkload
    ScenarioBehaviour
    Start
    Stop
    UsageModel
    UsageScenario
    UserData
    Workload
}

view Monitoring {
    ActiveResourceMeasuringPoint
    ActiveResourceReference
    Aggregation
    ArithmeticMean
    AssemblyOperationMeasuringPoint
    AssemblyPassiveResourceMeasuringPoint
    AssemblyReference
    EntryLevelSystemCallMeasuringPoint
    EntryLevelSystemCallReference
    ExternalCallActionMeasuringPoint
    ExternalCallActionReference
    FeedThrough
    FixedSizeAggregation
    GeometricMean
    HarmonicMean
    LinkingResourceMeasuringPoint
    LinkingResourceReference
    MeasurementDrivenAggregation
    MeasurementSpecification
    MeasuringPoint
    MeasuringPointRepository
    Median
    Monitor
    MonitorRepository
    OperationReference
    PassiveResourceReference
    ProcessingType
    ReconfigurationMeasuringPoint
    ResourceContainerMeasuringPoint
    ResourceContainerReference
    ResourceEnvironmentMeasuringPoint
    ResourceEnvironmentReference
    ResourceURIMeasuringPoint
    StatisticalCharacterization
    StringMeasuringPoint
    SubSystemOperationMeasuringPoint
    SubSystemReference
    SystemOperationMeasuringPoint
    SystemReference
    TimeDriven
    TimeDrivenAggregation
    UsageScenarioMeasuringPoint
    UsageScenarioReference
    VariableSizeAggregation
}
