// The Palladio metamodel's component types, interfaces, signatures, roles and data types.
//
// The base part of the metamodel, which ../palladio.mm imports.

// ---------------------------------------------------------------------------------------
// Enums

enum ResourceInterface { // elements of Palladio's library models
    CPU_INTERFACE
    HDD_INTERFACE
}

enum ParameterModifier {
    none
    `in`
    out
    inout
}

enum PrimitiveTypeEnum {
    INT
    STRING
    BOOL
    DOUBLE
    CHAR
    BYTE
    LONG
}

// ---------------------------------------------------------------------------------------
// Core: named elements and roles

abstract class InterfaceProvidingEntity extends NamedElement {}

abstract class InterfaceRequiringEntity extends ResourceInterfaceRequiringEntity {}

abstract class NamedElement {
    entityName: string[?]
}

class ResourceInterfaceRequiringEntity extends NamedElement {}

class ResourceProvidedRole extends Role {}

class ResourceRequiredRole extends Role {
    requiredResourceInterface: ResourceInterface[?]
}

InterfaceProvidingEntity.providedRoles[*] *--> ProvidedRole.providingEntity[?]
InterfaceRequiringEntity.requiredRoles[*] *--> RequiredRole.requiringEntity[?]
ResourceInterfaceRequiringEntity.resourceRequiredRoles[*] *--> ResourceRequiredRole.resourceInterfaceRequiringEntity[?]

// ---------------------------------------------------------------------------------------
// Repository: components, interfaces, signatures, data types

class CollectionDataType extends NamedElement, DataType {}

class CompleteComponentType extends RepositoryComponent {}

class CompositeDataType extends NamedElement, DataType {}

abstract class DataType {}

class EventGroup extends Interface {}

class EventType extends Signature {}

class InfrastructureInterface extends Interface {}

class InfrastructureProvidedRole extends ProvidedRole {}

class InfrastructureRequiredRole extends RequiredRole {}

class InfrastructureSignature extends Signature {}

class InnerDeclaration extends NamedElement {}

abstract class Interface extends NamedElement {}

class OperationInterface extends Interface {}

class OperationProvidedRole extends ProvidedRole {}

class OperationRequiredRole extends RequiredRole {}

class OperationSignature extends Signature {}

class Parameter {
    parameterName: string[?]
    modifier: ParameterModifier[?]
}

class PrimitiveDataType extends DataType {
    type: PrimitiveTypeEnum[?]
}

abstract class ProvidedRole extends Role {}

class ProvidesComponentType extends RepositoryComponent {}

class Repository extends NamedElement {}

abstract class RepositoryComponent extends InterfaceProvidingEntity, InterfaceRequiringEntity {}

abstract class RequiredRole extends Role {}

abstract class Role extends NamedElement {}

abstract class Signature extends NamedElement {}

class SinkRole extends ProvidedRole {}

class SourceRole extends RequiredRole {}

Parameter.dataType[?] --> DataType
Repository.components[*] *--> RepositoryComponent.repository[?]
Repository.interfaces[*] *--> Interface.repository[?]
Repository.dataTypes[*] *--> DataType.repository[?]
Interface.parentInterfaces[*] --> Interface
EventGroup.eventTypes[*] *--> EventType.eventGroup[?]
EventType.parameter[?] *--> Parameter.eventType[?]
InfrastructureSignature.parameters[*] *--> Parameter.infrastructureSignature[?]
InfrastructureInterface.infrastructureSignatures[*] *--> InfrastructureSignature.infrastructureInterface[?]
InfrastructureRequiredRole.requiredInterface[?] --> InfrastructureInterface
OperationSignature.parameters[*] *--> Parameter.operationSignature[?]
OperationSignature.returnType[?] --> DataType
OperationInterface.signatures[*] *--> OperationSignature.interface[?]
OperationRequiredRole.requiredInterface[?] --> OperationInterface
SourceRole.eventGroup[?] --> EventGroup
SinkRole.eventGroup[?] --> EventGroup
OperationProvidedRole.providedInterface[?] --> OperationInterface
InfrastructureProvidedRole.providedInterface[?] --> InfrastructureInterface
CollectionDataType.innerType[?] --> DataType
CompositeDataType.parentType[*] --> CompositeDataType
CompositeDataType.innerDeclaration[*] *--> InnerDeclaration.compositeDataType[?]
InnerDeclaration.datatype[?] --> DataType
