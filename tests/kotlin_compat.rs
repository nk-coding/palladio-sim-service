//! Wire compatibility with the platform's Kotlin protocol classes (kotlinx-serialization CBOR).
//! Ignored by default: run with `KCOMPAT_DIR=<dir> cargo test --test kotlin_compat -- --ignored`
//! around the Kotlin checker, which decodes `service.cbor` and writes `client.cbor(.model)`.

use palladio_sim_service::protocol::*;

#[test]
#[ignore]
fn write_service_messages() {
    let dir = std::env::var("KCOMPAT_DIR").expect("KCOMPAT_DIR");
    let series = WireValue::Record {
        class_name: "SimSeries".into(),
        fields: vec![
            ("measuringPoint".into(), WireValue::String("UsageScenarioMeasuringPoint[_us1]".into())),
            ("monitor".into(), WireValue::Null),
            ("count".into(), WireValue::Long(1 << 40)),
            ("mean".into(), WireValue::Double(0.1)),
            ("timeWeightedMean".into(), WireValue::Null),
        ],
    };
    let msg = ServiceMessage::Result {
        call_id: 1 << 33,
        objects: vec![HeapObject { id: -1, kind: HeapKind::List, version: 0, elements: Some(vec![series]), entries: None }],
        value: WireValue::Record {
            class_name: "SimResult".into(),
            fields: vec![
                ("series".into(), WireValue::Ref(-1)),
                ("replications".into(), WireValue::Int(3)),
                ("simulatedTime".into(), WireValue::Double(12.5)),
                ("f".into(), WireValue::Float(0.25)),
            ],
        },
    };
    std::fs::write(format!("{dir}/service.cbor"), encode_service(&msg)).unwrap();
    let fail = ServiceMessage::Failure { call_id: 2, message: "x".into(), code: None };
    std::fs::write(format!("{dir}/failure.cbor"), encode_service(&fail)).unwrap();
}

#[test]
#[ignore]
fn read_client_messages() {
    let dir = std::env::var("KCOMPAT_DIR").expect("KCOMPAT_DIR");
    let call = decode_client(&std::fs::read(format!("{dir}/client.cbor")).unwrap()).unwrap();
    let ClientMessage::Call { call_id, operation, objects, args, model_id } = call else { panic!() };
    assert_eq!((call_id, operation.as_str(), model_id), (9, "pcmStat", Some(4)));
    assert_eq!(objects[0].elements.as_ref().unwrap()[0], WireValue::Double(2.0));
    assert_eq!(
        args,
        vec![
            WireValue::String("x".into()),
            WireValue::Int(7),
            WireValue::Long(1 << 40),
            WireValue::Float(0.5),
            WireValue::Bool(true),
            WireValue::Null
        ]
    );
    let model = decode_client(&std::fs::read(format!("{dir}/client.cbor.model")).unwrap()).unwrap();
    let ClientMessage::Model { model_id: 4, model } = model else { panic!() };
    assert_eq!(model.instances[0].references, vec![("usageScenario".to_string(), vec!["b".to_string(), "c".to_string()])]);
    assert_eq!(model.instances[0].attributes, vec![("x".to_string(), vec![WireValue::String("E".into())])]);
}
