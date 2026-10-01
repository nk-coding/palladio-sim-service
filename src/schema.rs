//! The Palladio metamodel as mdeo sees it, and how it maps onto the PCM 5.2 Ecore metamodel.
//!
//! Everything here is derived from SimOxide's Ecore tables ([`simoxide_model::meta`]), restricted
//! to the classes and features SimOxide reads. The same [`Schema`] writes the `.mm` file
//! (`gen-metamodel`), turns PCM XMI into `.m` files (`xmi2mdeo`), and turns the models the
//! platform sends back into XMI for the simulator ([`crate::xmi`]), so the three cannot drift.
//!
//! The mapping rules:
//! - An EClass keeps its name. Abstract mixins that carry nothing the simulator reads are left
//!   out, and their subclasses extend the nearest ancestor that is kept.
//! - A feature is named after its EMF name up to the first `_` (`components__Repository` →
//!   `components`), unless that would clash within the class.
//! - `id` is not a property: the mdeo instance name is the XMI id.
//! - A contained `PCMRandomVariable` is a `string` property holding the StoEx
//!   (`processingRate = "1.0"`), and a contained variable reference (`NamespaceReference` /
//!   `VariableReference`) a `string` of dot-separated names (`"a.NUMBER_OF_ELEMENTS"`).
//! - A reference into Palladio's bundled library models (resource types, scheduling policies,
//!   resource interfaces and signatures, metric descriptions) is an enum property whose entries
//!   are the library's elements (`activeResourceType = ProcessingResourceType.CPU`).
//! - Every other reference is an association. Containment becomes a composition, navigable from
//!   the part through its EMF container feature (or a synthetic `<feature>Owner` when EMF has
//!   none); opposite references become one bidirectional association.

use simoxide_model::meta::{
    self, ClassId, DataKind, EnumId, FeatureId, FeatureKind, PackageId, class as c, feat as f,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

/// EMF features the simulator reads (SimOxide's model builder, validation and IR compiler),
/// plus the containments that hold the monitored measuring points. Features of the library
/// models (resource types, metric specifications) are not listed: those models are not edited.
const USED_FEATURES: &[FeatureId] = &[
    f::AbstractAction_predecessor_AbstractAction,
    f::AbstractAction_successor_AbstractAction,
    f::AbstractBranchTransition_branchBehaviour_BranchTransition,
    f::AbstractInternalControlFlowAction_infrastructureCall__Action,
    f::AbstractInternalControlFlowAction_resourceCall__Action,
    f::AbstractInternalControlFlowAction_resourceDemand_Action,
    f::AbstractLoopAction_bodyBehaviour_Loop,
    f::AbstractUserAction_predecessor,
    f::AbstractUserAction_successor,
    f::AcquireAction_passiveresource_AcquireAction,
    f::AcquireAction_timeout,
    f::AcquireAction_timeoutValue,
    f::ActiveResourceReference_activeResource,
    f::ActiveResourceReference_replicaID,
    f::Aggregation_statisticalCharacterization,
    f::Allocation_allocationContexts_Allocation,
    f::AllocationContext_assemblyContext_AllocationContext,
    f::AllocationContext_resourceContainer_AllocationContext,
    f::Allocation_system_Allocation,
    f::Allocation_targetResourceEnvironment_Allocation,
    f::AssemblyConnector_providedRole_AssemblyConnector,
    f::AssemblyConnector_providingAssemblyContext_AssemblyConnector,
    f::AssemblyConnector_requiredRole_AssemblyConnector,
    f::AssemblyConnector_requiringAssemblyContext_AssemblyConnector,
    f::AssemblyContext_configParameterUsages__AssemblyContext,
    f::AssemblyContext_encapsulatedComponent__AssemblyContext,
    f::AssemblyInfrastructureConnector_providedRole__AssemblyInfrastructureConnector,
    f::AssemblyInfrastructureConnector_providingAssemblyContext__AssemblyInfrastructureConnector,
    f::AssemblyInfrastructureConnector_requiredRole__AssemblyInfrastructureConnector,
    f::AssemblyInfrastructureConnector_requiringAssemblyContext__AssemblyInfrastructureConnector,
    f::AssemblyReference_assembly,
    f::BasicComponent_passiveResource_BasicComponent,
    f::BasicComponent_serviceEffectSpecifications__BasicComponent,
    f::BranchAction_branches_Branch,
    f::Branch_branchTransitions_Branch,
    f::BranchTransition_branchedBehaviour_BranchTransition,
    f::BranchTransition_branchProbability,
    f::CallAction_inputVariableUsages__CallAction,
    f::CallReturnAction_returnVariableUsage__CallReturnAction,
    f::ClosedWorkload_population,
    f::ClosedWorkload_thinkTime_ClosedWorkload,
    f::CollectionDataType_innerType_CollectionDataType,
    f::CollectionIteratorAction_parameter_CollectionIteratorAction,
    f::CommunicationLinkResourceSpecification_communicationLinkResourceType_CommunicationLinkResourceSpecification,
    f::CommunicationLinkResourceSpecification_failureProbability,
    f::CommunicationLinkResourceSpecification_latency_CommunicationLinkResourceSpecification,
    f::CommunicationLinkResourceSpecification_throughput_CommunicationLinkResourceSpecification,
    f::ComposedStructure_assemblyContexts__ComposedStructure,
    f::ComposedStructure_connectors__ComposedStructure,
    f::CompositeDataType_innerDeclaration_CompositeDataType,
    f::CompositeDataType_parentType_CompositeDataType,
    f::Delay_timeSpecification_Delay,
    f::EmitEventAction_eventType__EmitEventAction,
    f::EmitEventAction_sourceRole__EmitEventAction,
    f::EntryLevelSystemCall_inputParameterUsages_EntryLevelSystemCall,
    f::EntryLevelSystemCall_operationSignature__EntryLevelSystemCall,
    f::EntryLevelSystemCall_outputParameterUsages_EntryLevelSystemCall,
    f::EntryLevelSystemCall_priority,
    f::EntryLevelSystemCall_providedRole_EntryLevelSystemCall,
    f::EntryLevelSystemCallReference_entryLevelSystemCall,
    f::EventGroup_eventTypes__EventGroup,
    f::EventType_parameter__EventType,
    f::ExternalCallAction_calledService_ExternalService,
    f::ExternalCallActionReference_externalCall,
    f::ExternalCallAction_retryCount,
    f::ExternalCallAction_role_ExternalService,
    f::FixedSizeAggregation_numberOfMeasurements,
    f::ForkAction_asynchronousForkedBehaviours_ForkAction,
    f::ForkAction_synchronisingBehaviours_ForkAction,
    f::GuardedBranchTransition_branchCondition_GuardedBranchTransition,
    f::HDDProcessingResourceSpecification_readProcessingRate,
    f::HDDProcessingResourceSpecification_writeProcessingRate,
    f::ImplementationComponentType_componentParameterUsage_ImplementationComponentType,
    f::ImplementationComponentType_componentType,
    f::InfrastructureCall_numberOfCalls__InfrastructureCall,
    f::InfrastructureCall_requiredRole__InfrastructureCall,
    f::InfrastructureCall_signature__InfrastructureCall,
    f::InfrastructureInterface_infrastructureSignatures__InfrastructureInterface,
    f::InfrastructureProvidedRole_providedInterface__InfrastructureProvidedRole,
    f::InfrastructureRequiredRole_requiredInterface__InfrastructureRequiredRole,
    f::InfrastructureSignature_parameters__InfrastructureSignature,
    f::InnerDeclaration_datatype_InnerDeclaration,
    f::Interface_parentInterfaces__Interface,
    f::InterfaceProvidingEntity_providedRoles_InterfaceProvidingEntity,
    f::InterfaceRequiringEntity_requiredRoles_InterfaceRequiringEntity,
    f::InternalCallAction_calledResourceDemandingInternalBehaviour,
    f::LinkingResource_communicationLinkResourceSpecifications_LinkingResource,
    f::LinkingResource_connectedResourceContainers_LinkingResource,
    f::LinkingResourceReference_linkingResource,
    f::LoopAction_iterationCount_LoopAction,
    f::Loop_bodyBehaviour_Loop,
    f::Loop_loopIteration_Loop,
    f::MeasurementDrivenAggregation_frequency,
    f::MeasurementSpecification_metricDescription,
    f::MeasurementSpecification_processingType,
    f::MeasurementSpecification_triggersSelfAdaptations,
    f::MeasuringPointRepository_measuringPoints,
    f::MeasuringPoint_stringRepresentation,
    f::Monitor_activated,
    f::Monitor_measurementSpecifications,
    f::Monitor_measuringPoint,
    f::MonitorRepository_monitors,
    f::NamedElement_entityName,
    f::OperationInterface_signatures__OperationInterface,
    f::OperationProvidedRole_providedInterface__OperationProvidedRole,
    f::OperationReference_operationSignature,
    f::OperationReference_role,
    f::OperationRequiredRole_requiredInterface__OperationRequiredRole,
    f::OperationSignature_parameters__OperationSignature,
    f::OperationSignature_returnType__OperationSignature,
    f::Parameter_dataType__Parameter,
    f::Parameter_modifier__Parameter,
    f::Parameter_parameterName,
    f::ParametricResourceDemand_requiredResource_ParametricResourceDemand,
    f::ParametricResourceDemand_specification_ParametericResourceDemand,
    f::PassiveResource_capacity_PassiveResource,
    f::PassiveResourceReference_passiveResource,
    f::PrimitiveDataType_type,
    f::ProbabilisticBranchTransition_branchProbability,
    f::ProcessingResourceSpecification_activeResourceType_ActiveResourceSpecification,
    f::ProcessingResourceSpecification_MTTF,
    f::ProcessingResourceSpecification_MTTR,
    f::ProcessingResourceSpecification_numberOfReplicas,
    f::ProcessingResourceSpecification_processingRate_ProcessingResourceSpecification,
    f::ProcessingResourceSpecification_requiredByContainer,
    f::ProcessingResourceSpecification_schedulingPolicy,
    f::ProvidedDelegationConnector_assemblyContext_ProvidedDelegationConnector,
    f::ProvidedDelegationConnector_innerProvidedRole_ProvidedDelegationConnector,
    f::ProvidedDelegationConnector_outerProvidedRole_ProvidedDelegationConnector,
    f::ProvidedInfrastructureDelegationConnector_assemblyContext__ProvidedInfrastructureDelegationConnector,
    f::ProvidedInfrastructureDelegationConnector_innerProvidedRole__ProvidedInfrastructureDelegationConnector,
    f::ProvidedInfrastructureDelegationConnector_outerProvidedRole__ProvidedInfrastructureDelegationConnector,
    f::repository_Repository_components__Repository,
    f::repository_Repository_dataTypes__Repository,
    f::repository_Repository_interfaces__Repository,
    f::RequiredDelegationConnector_assemblyContext_RequiredDelegationConnector,
    f::RequiredDelegationConnector_innerRequiredRole_RequiredDelegationConnector,
    f::RequiredDelegationConnector_outerRequiredRole_RequiredDelegationConnector,
    f::RequiredInfrastructureDelegationConnector_assemblyContext__RequiredInfrastructureDelegationConnector,
    f::RequiredInfrastructureDelegationConnector_innerRequiredRole__RequiredInfrastructureDelegationConnector,
    f::RequiredInfrastructureDelegationConnector_outerRequiredRole__RequiredInfrastructureDelegationConnector,
    f::RequiredResourceDelegationConnector_assemblyContext__RequiredResourceDelegationConnector,
    f::RequiredResourceDelegationConnector_innerRequiredRole__RequiredResourceDelegationConnector,
    f::RequiredResourceDelegationConnector_outerRequiredRole__RequiredResourceDelegationConnector,
    f::ResourceCall_numberOfCalls__ResourceCall,
    f::ResourceCall_resourceRequiredRole__ResourceCall,
    f::ResourceCall_signature__ResourceCall,
    f::ResourceContainer_activeResourceSpecifications_ResourceContainer,
    f::ResourceContainer_nestedResourceContainers__ResourceContainer,
    f::ResourceContainerReference_resourceContainer,
    f::ResourceDemandingBehaviour_steps_Behaviour,
    f::ResourceDemandingSEFF_resourceDemandingInternalBehaviours,
    f::ResourceEnvironment_linkingResources__ResourceEnvironment,
    f::ResourceEnvironmentReference_resourceEnvironment,
    f::ResourceEnvironment_resourceContainer_ResourceEnvironment,
    f::ResourceInterfaceRequiringEntity_resourceRequiredRoles__ResourceInterfaceRequiringEntity,
    f::ResourceRequiredRole_requiredResourceInterface__ResourceRequiredRole,
    f::ResourceURIMeasuringPoint_resourceURI,
    f::ScenarioBehaviour_actions_ScenarioBehaviour,
    f::ServiceEffectSpecification_describedService__SEFF,
    f::ServiceEffectSpecification_seffTypeID,
    f::SetVariableAction_localVariableUsages_SetVariableAction,
    f::SinkRole_eventGroup__SinkRole,
    f::SourceRole_eventGroup__SourceRole,
    f::StringMeasuringPoint_measuringPoint,
    f::SubSystemReference_subsystem,
    f::SynchronisationPoint_outputParameterUsage_SynchronisationPoint,
    f::SynchronisationPoint_synchronousForkedBehaviours_SynchronisationPoint,
    f::SystemReference_system,
    f::TimeDriven_windowIncrement,
    f::TimeDriven_windowLength,
    f::UsageModel_usageScenario_UsageModel,
    f::UsageModel_userData_UsageModel,
    f::UsageScenarioReference_usageScenario,
    f::UsageScenario_scenarioBehaviour_UsageScenario,
    f::UsageScenario_workload_UsageScenario,
    f::UserData_assemblyContext_userData,
    f::UserData_userDataParameterUsages_UserData,
    f::VariableCharacterisation_specification_VariableCharacterisation,
    f::VariableCharacterisation_type,
    f::VariableSizeAggregation_retrospectionLength,
    f::VariableUsage_namedReference__VariableUsage,
    f::VariableUsage_variableCharacterisation_VariableUsage,
    f::MeasuringPoint_resourceURIRepresentation,
    f::OpenWorkload_interArrivalTime_OpenWorkload,
    f::ReleaseAction_passiveResource_ReleaseAction,
    // Without failure simulation, a recovery action runs its primary behaviour.
    f::RecoveryAction_recoveryActionBehaviours__RecoveryAction,
    f::RecoveryAction_primaryBehaviour__RecoveryAction,
];

/// Packages whose classes users model. Everything else is either a library (resource types,
/// metric specifications), a flattened helper (StoEx, identifiers) or out of the simulator's
/// scope (reliability, QoS annotations, protocols).
const MODELLED_PACKAGES: &[&str] = &[
    "pcm.core",
    "pcm.core.entity",
    "pcm.core.composition",
    "pcm.usagemodel",
    "pcm.repository",
    "pcm.parameter",
    "pcm.seff",
    "pcm.seff.seff_performance",
    "pcm.seff.seff_reliability",
    "pcm.system",
    "pcm.resourceenvironment",
    "pcm.allocation",
    "pcm.subsystem",
    "models.measuringpoint",
    "monitorrepository",
    "pcmmeasuringpoint",
    "simulizarmeasuringpoint",
];

/// A root class, the file extension its models are saved with, and the name of its `.mm` group.
pub const ROOTS: &[(ClassId, &str)] = &[
    (c::repository_Repository, "repository"),
    (c::System, "system"),
    (c::ResourceEnvironment, "resourceenvironment"),
    (c::Allocation, "allocation"),
    (c::UsageModel, "usagemodel"),
    (c::MonitorRepository, "monitorrepository"),
    (c::MeasuringPointRepository, "measuringpoint"),
];

/// The library models whose elements references into them may name, with the URI models use
/// to refer to them.
pub const LIBRARIES: &[&str] = &[
    "pathmap://PCM_MODELS/Palladio.resourcetype",
    "pathmap://METRIC_SPEC_MODELS/models/commonMetrics.metricspec",
];

/// The library holding the primitive data types. Models refer into it, but its types become
/// ordinary `PrimitiveDataType` instances of the model's repository.
pub const PRIMITIVE_TYPES: &str = "pathmap://PCM_MODELS/PrimitiveTypes.repository";

/// What an mdeo attribute holds.
#[derive(Clone, Debug, PartialEq)]
pub enum AttrType {
    Int,
    Long,
    Double,
    Bool,
    String,
    /// An enum of the metamodel ([`Schema::enums`]).
    Enum(usize),
}

impl AttrType {
    pub fn mm_name<'a>(&self, schema: &'a Schema) -> &'a str {
        match self {
            AttrType::Int => "int",
            AttrType::Long => "long",
            AttrType::Double => "double",
            AttrType::Bool => "boolean",
            AttrType::String => "string",
            AttrType::Enum(e) => &schema.enums[*e].name,
        }
    }
}

/// How an mdeo property maps onto EMF.
#[derive(Clone, Debug)]
pub enum PropKind {
    /// An EMF attribute. Enum values are EMF literals by entry name.
    Attr { ty: AttrType },
    /// A contained `PCMRandomVariable`, as its StoEx specification.
    RandomVar,
    /// A contained `AbstractNamedReference`, as dot-separated reference names.
    NamedRef,
    /// A reference to an element of a library model, as an entry of the library enum.
    Library { lib_enum: usize },
    /// A reference to other instances (an association end).
    Ref { target: usize, containment: bool },
    /// The part side of a containment: never written to XMI, the nesting says it.
    Container { target: usize },
}

#[derive(Clone, Debug)]
pub struct Prop {
    pub name: String,
    /// The EMF feature; `None` for a synthetic container end.
    pub feature: Option<FeatureId>,
    pub kind: PropKind,
    pub many: bool,
}

#[derive(Clone, Debug)]
pub struct MClass {
    pub name: String,
    pub eclass: ClassId,
    pub is_abstract: bool,
    /// Direct mdeo superclasses (indices into [`Schema::classes`]).
    pub supers: Vec<usize>,
    /// Properties declared by this class itself.
    pub own: Vec<usize>,
}

/// An enum of the metamodel.
#[derive(Clone, Debug)]
pub struct MEnum {
    pub name: String,
    pub entries: Vec<EnumEntry>,
    pub source: EnumSource,
}

#[derive(Clone, Debug)]
pub struct EnumEntry {
    pub name: String,
    /// EMF literal (for an Ecore enum) or `uri#fragment` of the library element.
    pub value: String,
    /// The library element's class, for the `xsi:type` of the reference.
    pub class: Option<ClassId>,
}

#[derive(Clone, Debug)]
pub enum EnumSource {
    Ecore(EnumId),
    /// The elements of the library models that are instances of this class.
    Library(ClassId),
}

/// One association of the `.mm`: `source.name[mult] <op> target.name[mult]`.
#[derive(Clone, Debug)]
pub struct Assoc {
    pub source: usize,
    pub source_prop: Option<usize>,
    pub operator: &'static str,
    pub target: usize,
    pub target_prop: Option<usize>,
}

pub struct Schema {
    pub classes: Vec<MClass>,
    pub props: Vec<Prop>,
    pub enums: Vec<MEnum>,
    pub assocs: Vec<Assoc>,
    class_by_name: HashMap<String, usize>,
    class_by_eclass: HashMap<ClassId, usize>,
    /// Every property of a class, inherited ones included, by name.
    all_props: Vec<HashMap<String, usize>>,
    /// Every property of a class by EMF feature.
    by_feature: Vec<HashMap<FeatureId, usize>>,
    /// Library element `uri#fragment` → (enum, entry).
    library_entries: HashMap<String, (usize, usize)>,
}

/// The schema, built once per process.
pub fn schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(Schema::build)
}

fn package_name(p: PackageId) -> &'static str {
    p.def().name
}

fn modelled(cl: ClassId) -> bool {
    MODELLED_PACKAGES.contains(&package_name(cl.def().package))
}

fn is_library_class(cl: ClassId) -> bool {
    matches!(package_name(cl.def().package), "pcm.resourcetype" | "metricspec")
}

fn all_classes() -> impl Iterator<Item = ClassId> {
    (0..meta::CLASSES.len()).map(|i| ClassId(i as u32))
}

/// Concrete (or all) subclasses of `cl`, itself included.
fn subclasses(cl: ClassId) -> impl Iterator<Item = ClassId> {
    all_classes().filter(move |x| x.is_a(cl))
}

/// The mdeo name of an EMF feature: its name up to the first `_`.
pub fn short_name(name: &str) -> &str {
    match name.char_indices().skip(1).find(|&(_, ch)| ch == '_') {
        Some((i, _)) => &name[..i],
        None => name,
    }
}

/// `Processor Sharing` → `PROCESSOR_SHARING`, `CpuInterface` → `CPU_INTERFACE`.
pub fn upper_snake(s: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            if ch.is_ascii_uppercase() && prev_lower {
                out.push('_');
            }
            out.push(ch.to_ascii_uppercase());
            prev_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            prev_lower = false;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

impl Schema {
    fn build() -> Schema {
        let used: HashSet<FeatureId> = USED_FEATURES.iter().copied().collect();
        let is_random_var = |t: ClassId| t.is_a(c::RandomVariable);
        let is_named_ref = |t: ClassId| t.is_a(c::AbstractNamedReference);

        // Classes users model: the roots, and whatever their used features reach, with every
        // subclass in a modelled package (a feature typed `AbstractAction` holds any action).
        let mut included: Vec<ClassId> = Vec::new();
        let mut seen: HashSet<ClassId> = HashSet::new();
        let mut queue: Vec<ClassId> = ROOTS.iter().map(|&(cl, _)| cl).collect();
        while let Some(cl) = queue.pop() {
            if !seen.insert(cl) {
                continue;
            }
            included.push(cl);
            for fid in cl.features() {
                if !used.contains(&fid) {
                    continue;
                }
                let Some(t) = fid.target() else { continue };
                if fid.is_container() || is_random_var(t) || is_named_ref(t) || is_library_class(t) {
                    continue;
                }
                for sub in subclasses(t) {
                    if modelled(sub) && (!sub.def().is_abstract || sub == t) && !seen.contains(&sub) {
                        queue.push(sub);
                    }
                }
            }
        }
        // Abstract ancestors are kept when they declare something used, so the property sits
        // where EMF declares it.
        let mut keep: HashSet<ClassId> = included.iter().copied().collect();
        for &cl in &included {
            for &sup in cl.def().all_supers {
                if modelled(sup) && sup.features().any(|f| f.def().owner == sup && used.contains(&f)) {
                    keep.insert(sup);
                }
            }
        }
        let mut classes_e: Vec<ClassId> = keep.into_iter().collect();
        classes_e.sort_by_key(|cl| cl.0);

        let mut classes: Vec<MClass> = Vec::new();
        let mut class_by_eclass = HashMap::new();
        let mut class_by_name: HashMap<String, usize> = HashMap::new();
        let names: HashMap<&str, usize> = classes_e.iter().fold(HashMap::new(), |mut m, cl| {
            *m.entry(cl.name()).or_default() += 1;
            m
        });
        for &cl in &classes_e {
            let name = if names[cl.name()] > 1 {
                format!("{}_{}", cl.def().package.short_name(), cl.name())
            } else {
                cl.name().to_string()
            };
            class_by_eclass.insert(cl, classes.len());
            class_by_name.insert(name.clone(), classes.len());
            classes.push(MClass {
                name,
                eclass: cl,
                is_abstract: cl.def().is_abstract,
                supers: Vec::new(),
                own: Vec::new(),
            });
        }
        // Direct supers: the nearest kept ancestors, without ones implied by another.
        for i in 0..classes.len() {
            let mut found: Vec<ClassId> = Vec::new();
            let mut stack: Vec<ClassId> = classes[i].eclass.def().supers.to_vec();
            while let Some(s) = stack.pop() {
                if class_by_eclass.contains_key(&s) {
                    if !found.contains(&s) {
                        found.push(s);
                    }
                } else {
                    stack.extend(s.def().supers.iter().copied());
                }
            }
            let direct: Vec<ClassId> = found
                .iter()
                .copied()
                .filter(|&s| !found.iter().any(|&o| o != s && o.is_a(s)))
                .collect();
            let mut supers: Vec<usize> = direct.iter().map(|s| class_by_eclass[s]).collect();
            supers.sort();
            classes[i].supers = supers;
        }

        let mut s = Schema {
            classes,
            props: Vec::new(),
            enums: Vec::new(),
            assocs: Vec::new(),
            class_by_name,
            class_by_eclass,
            all_props: Vec::new(),
            by_feature: Vec::new(),
            library_entries: HashMap::new(),
        };

        // Enums: Ecore enums used by attributes, and one per library class referenced.
        let mut ecore_enums: HashMap<EnumId, usize> = HashMap::new();
        let mut lib_enums: HashMap<ClassId, usize> = HashMap::new();
        let libraries = load_libraries();

        // Properties, in EMF feature order per class.
        let mut containments: Vec<(usize, usize)> = Vec::new(); // (owner class, prop)
        for ci in 0..s.classes.len() {
            let cl = s.classes[ci].eclass;
            for fid in cl.features() {
                let def = fid.def();
                if def.owner != cl || !used.contains(&fid) {
                    continue;
                }
                let kind = match def.kind {
                    FeatureKind::Attribute { data } => {
                        let ty = match data {
                            DataKind::Str | DataKind::Other => AttrType::String,
                            DataKind::Int => AttrType::Int,
                            DataKind::Long => AttrType::Long,
                            DataKind::Double => AttrType::Double,
                            DataKind::Bool => AttrType::Bool,
                            DataKind::Enum(e) => AttrType::Enum(*ecore_enums.entry(e).or_insert_with(|| {
                                s.enums.push(MEnum {
                                    name: e.def().name.to_string(),
                                    entries: e
                                        .def()
                                        .literals
                                        .iter()
                                        .map(|l| EnumEntry {
                                            name: l.name.to_string(),
                                            value: l.literal.to_string(),
                                            class: None,
                                        })
                                        .collect(),
                                    source: EnumSource::Ecore(e),
                                });
                                s.enums.len() - 1
                            })),
                        };
                        PropKind::Attr { ty }
                    }
                    FeatureKind::Reference { target, containment, container, .. } => {
                        if container || target.0 == u32::MAX {
                            continue;
                        }
                        if is_random_var(target) {
                            PropKind::RandomVar
                        } else if is_named_ref(target) {
                            PropKind::NamedRef
                        } else if is_library_class(target) {
                            let e = *lib_enums.entry(target).or_insert_with(|| {
                                s.enums.push(MEnum {
                                    name: target.name().to_string(),
                                    entries: library_entries(&libraries, target),
                                    source: EnumSource::Library(target),
                                });
                                s.enums.len() - 1
                            });
                            PropKind::Library { lib_enum: e }
                        } else {
                            let Some(&t) = s.class_by_eclass.get(&target) else {
                                continue;
                            };
                            PropKind::Ref { target: t, containment }
                        }
                    }
                };
                let pi = s.props.len();
                s.props.push(Prop {
                    name: short_name(def.name).to_string(),
                    feature: Some(fid),
                    kind: kind.clone(),
                    many: def.many,
                });
                s.classes[ci].own.push(pi);
                if let PropKind::Ref { containment: true, .. } = kind {
                    containments.push((ci, pi));
                }
            }
        }
        // Container ends: the EMF container feature, or a synthetic one.
        let mut container_of: HashMap<usize, usize> = HashMap::new(); // containment prop → container prop
        for &(owner, pi) in &containments {
            let fid = s.props[pi].feature.unwrap();
            let PropKind::Ref { target, .. } = s.props[pi].kind else { unreachable!() };
            let (name, feature) = match fid.opposite() {
                Some(o) => (short_name(o.name()).to_string(), Some(o)),
                None => (format!("{}Owner", s.props[pi].name), None),
            };
            // The container feature is declared on the EMF target class, which is kept since
            // it is the declared type of a used containment.
            let ci = if let Some(o) = feature {
                s.class_by_eclass[&o.def().owner]
            } else {
                target
            };
            let cp = s.props.len();
            s.props.push(Prop { name, feature, kind: PropKind::Container { target: owner }, many: false });
            s.classes[ci].own.push(cp);
            container_of.insert(pi, cp);
        }

        s.resolve_names();
        s.index();
        for (ei, e) in s.enums.iter().enumerate() {
            for (ni, n) in e.entries.iter().enumerate() {
                if n.class.is_some() {
                    s.library_entries.insert(n.value.clone(), (ei, ni));
                }
            }
        }

        // Associations: one per containment, one per pair of opposites, one per other reference.
        let mut paired: HashSet<usize> = HashSet::new();
        for ci in 0..s.classes.len() {
            for &pi in &s.classes[ci].own.clone() {
                let p = &s.props[pi];
                let PropKind::Ref { target, containment } = p.kind else { continue };
                if containment {
                    let cp = container_of[&pi];
                    let holder = match s.props[cp].feature {
                        Some(o) => s.class_by_eclass[&o.def().owner],
                        None => target,
                    };
                    s.assocs.push(Assoc {
                        source: ci,
                        source_prop: Some(pi),
                        operator: "*-->",
                        target: holder,
                        target_prop: Some(cp),
                    });
                    continue;
                }
                if paired.contains(&pi) {
                    continue;
                }
                let opposite = p.feature.and_then(|f| f.opposite()).and_then(|o| {
                    let oc = *s.class_by_eclass.get(&o.def().owner)?;
                    let op = *s.by_feature[oc].get(&o)?;
                    Some((oc, op))
                });
                match opposite {
                    Some((oc, op)) => {
                        paired.insert(op);
                        s.assocs.push(Assoc {
                            source: ci,
                            source_prop: Some(pi),
                            operator: "<-->",
                            target: oc,
                            target_prop: Some(op),
                        });
                    }
                    None => s.assocs.push(Assoc {
                        source: ci,
                        source_prop: Some(pi),
                        operator: "-->",
                        target,
                        target_prop: None,
                    }),
                }
            }
        }
        s
    }

    /// Makes property names unique within every class, inherited ones included: a name that
    /// clashes falls back to the full EMF feature name.
    fn resolve_names(&mut self) {
        loop {
            let mut changed = false;
            for ci in 0..self.classes.len() {
                let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
                for pi in self.inherited_props(ci) {
                    by_name.entry(self.props[pi].name.clone()).or_default().push(pi);
                }
                for (_, ps) in by_name {
                    if ps.len() < 2 {
                        continue;
                    }
                    for pi in ps {
                        let full = match (self.props[pi].feature, &self.props[pi].kind) {
                            (Some(f), _) => f.name().to_string(),
                            (None, _) => format!("{}_{}", self.props[pi].name, pi),
                        };
                        if self.props[pi].name != full {
                            self.props[pi].name = full;
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn inherited_props(&self, ci: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![ci];
        while let Some(c) = stack.pop() {
            if !seen.insert(c) {
                continue;
            }
            out.extend(self.classes[c].own.iter().copied());
            stack.extend(self.classes[c].supers.iter().copied());
        }
        out
    }

    fn index(&mut self) {
        self.all_props = (0..self.classes.len())
            .map(|ci| self.inherited_props(ci).into_iter().map(|pi| (self.props[pi].name.clone(), pi)).collect())
            .collect();
        self.by_feature = (0..self.classes.len())
            .map(|ci| {
                self.inherited_props(ci)
                    .into_iter()
                    .filter_map(|pi| Some((self.props[pi].feature?, pi)))
                    .collect()
            })
            .collect();
    }

    pub fn class(&self, name: &str) -> Option<usize> {
        self.class_by_name.get(name).copied()
    }

    pub fn class_of(&self, eclass: ClassId) -> Option<usize> {
        self.class_by_eclass.get(&eclass).copied()
    }

    /// A property of class `ci` (inherited ones included) by mdeo name.
    pub fn prop(&self, ci: usize, name: &str) -> Option<usize> {
        self.all_props[ci].get(name).copied()
    }

    /// A property of class `ci` (inherited ones included) by EMF feature.
    pub fn prop_by_feature(&self, ci: usize, f: FeatureId) -> Option<usize> {
        self.by_feature[ci].get(&f).copied()
    }

    /// Every property of class `ci`, inherited ones included.
    pub fn props_of(&self, ci: usize) -> impl Iterator<Item = usize> + '_ {
        self.all_props[ci].values().copied()
    }

    /// The enum entry naming a library element (`uri#fragment`).
    pub fn library_entry(&self, uri: &str) -> Option<(usize, usize)> {
        self.library_entries.get(uri).copied()
    }

    /// The file extension a root class's instance is saved with, if it is a root class.
    pub fn root_extension(&self, ci: usize) -> Option<&'static str> {
        let cl = self.classes[ci].eclass;
        ROOTS.iter().find(|(r, _)| cl.is_a(*r)).map(|&(_, ext)| ext)
    }

    /// The metamodel's `.mm` files, by path relative to the aggregate `palladio.mm`.
    pub fn metamodel_files(&self) -> Vec<(String, String)> {
        crate::mm::write(self)
    }
}

/// The library models, loaded from SimOxide's bundled copies.
pub struct Libraries {
    pub graph: simoxide_model::raw::Graph,
}

pub fn load_libraries() -> Libraries {
    let mut l = simoxide_model::Loader::default();
    for uri in LIBRARIES {
        l.load_uri(uri);
    }
    let (graph, _) = l.finish();
    Libraries { graph }
}

fn library_entries(libs: &Libraries, cl: ClassId) -> Vec<EnumEntry> {
    let g = &libs.graph;
    let mut out: Vec<EnumEntry> = Vec::new();
    for (ri, r) in g.resources.iter().enumerate() {
        if !r.uri.starts_with("pathmap:") {
            continue;
        }
        for o in g.all_contents(simoxide_model::raw::ResId(ri as u32)) {
            let obj = g.obj(o);
            if !obj.class.is_a(cl) || obj.proxy.is_some() {
                continue;
            }
            let label = ["entityName", "name"]
                .iter()
                .find_map(|n| obj.class.feature(n).and_then(|f| g.str_attr(o, f)))
                .map(|s| s.to_string())
                .unwrap_or_else(|| g.fragment(o));
            let mut name = upper_snake(&label);
            if out.iter().any(|e| e.name == name) {
                name = format!("{name}_{}", out.len());
            }
            out.push(EnumEntry {
                name,
                value: format!("{}#{}", r.uri, g.fragment(o)),
                class: Some(obj.class),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(short_name("components__Repository"), "components");
        assert_eq!(short_name("entityName"), "entityName");
        assert_eq!(short_name("MTTF"), "MTTF");
        assert_eq!(upper_snake("Processor Sharing"), "PROCESSOR_SHARING");
        assert_eq!(upper_snake("CpuInterface"), "CPU_INTERFACE");
        assert_eq!(upper_snake("First-Come-First-Serve"), "FIRST_COME_FIRST_SERVE");
    }

    #[test]
    fn schema_is_consistent() {
        let s = schema();
        for (ci, cl) in s.classes.iter().enumerate() {
            let mut names = HashSet::new();
            for pi in s.inherited_props(ci) {
                assert!(names.insert(s.props[pi].name.clone()), "{}.{} twice", cl.name, s.props[pi].name);
            }
        }
        let cpu = s.enums.iter().find(|e| e.name == "ProcessingResourceType").unwrap();
        assert!(cpu.entries.iter().any(|e| e.name == "CPU"));
    }
}
