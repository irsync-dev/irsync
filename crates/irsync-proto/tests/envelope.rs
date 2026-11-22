//! irsync's capture `Record` must keep fitting the `pbcap` envelope.

use prost_reflect::{Cardinality, DescriptorPool, FieldDescriptor, Kind, MessageDescriptor};

fn pool(bytes: &[u8]) -> DescriptorPool {
    DescriptorPool::decode(bytes).expect("descriptor set decodes")
}

fn shape(f: &FieldDescriptor) -> (Cardinality, bool) {
    (f.cardinality(), f.is_list())
}

/// Same number, kind and cardinality; messages and enums are compared
/// by the caller.
fn same_scalar(a: &FieldDescriptor, b: &FieldDescriptor) {
    assert_eq!(a.number(), b.number());
    assert_eq!(
        std::mem::discriminant(&a.kind()),
        std::mem::discriminant(&b.kind()),
        "kind of field {}",
        a.number()
    );
    assert_eq!(shape(a), shape(b), "cardinality of field {}", a.number());
}

fn same_message(a: &MessageDescriptor, b: &MessageDescriptor) {
    assert_eq!(a.fields().count(), b.fields().count(), "{}", a.full_name());
    for fb in b.fields() {
        let fa = a
            .get_field(fb.number())
            .unwrap_or_else(|| panic!("{} lacks field {}", a.full_name(), fb.number()));
        same_scalar(&fa, &fb);
        if let (Kind::Message(ma), Kind::Message(mb)) = (fa.kind(), fb.kind()) {
            same_message(&ma, &mb);
        }
    }
}

#[test]
#[cfg_attr(miri, ignore = "descriptor comparison; no unsafe code involved")]
fn record_fits_the_envelope() {
    let irsync = pool(irsync_proto::FILE_DESCRIPTOR_SET);
    let pbcap = pool(pbcap::FILE_DESCRIPTOR_SET);
    let record = irsync
        .get_message_by_name("irsync.capture.v1.Record")
        .expect("Record");
    let envelope = pbcap
        .get_message_by_name("pbcap.v1.Envelope")
        .expect("Envelope");

    for n in [2, 10, 11] {
        let r = record.get_field(n).expect("Record field");
        let e = envelope.get_field(n).expect("Envelope field");
        same_scalar(&r, &e);
        match (r.kind(), e.kind()) {
            (Kind::Enum(re), Kind::Enum(ee)) => {
                let values = |d: &prost_reflect::EnumDescriptor| {
                    d.values()
                        .map(|v| (v.name().to_owned(), v.number()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(values(&re), values(&ee));
            }
            (Kind::Message(rm), Kind::Message(em)) => same_message(&rm, &em),
            _ => {}
        }
    }

    // Every field numbered 20 or more is a singular message.
    for f in record.fields().filter(|f| f.number() >= 20) {
        assert!(
            matches!(f.kind(), Kind::Message(_)) && !f.is_list() && !f.is_map(),
            "field {} must be a singular message",
            f.name()
        );
    }

    // 20..=39 are exactly the msg oneof members other than the two events.
    let oneof = record
        .oneofs()
        .find(|o| o.name() == "msg")
        .expect("oneof msg");
    let mut wire: Vec<u32> = oneof
        .fields()
        .filter(|f| !matches!(f.name(), "stream_event" | "path_change"))
        .map(|f| f.number())
        .collect();
    wire.sort_unstable();
    let mut in_range: Vec<u32> = record
        .fields()
        .map(|f| f.number())
        .filter(|n| pbcap::MESSAGE_FIELDS.contains(n))
        .collect();
    in_range.sort_unstable();
    assert_eq!(wire, in_range);
    for name in ["stream_event", "path_change"] {
        let f = record.get_field_by_name(name).expect(name);
        assert!(f.number() >= pbcap::FIRST_EVENT_FIELD, "{name}");
    }
}
