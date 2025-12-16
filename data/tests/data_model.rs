use std::cmp::Ordering;

use data::{
    entity::Entity,
    model_utils::generate_id,
    netmsg::{Header, Message, MessageType},
    task::Task,
    task_compare::{SortField, TaskComparator},
    value::Value,
};

#[test]
fn entity_hash_changes_when_properties_change() {
    let e1 = Entity::new(1, 2, "owner", 10);
    let mut e2 = Entity::new(1, 2, "owner", 10);
    assert_eq!(e1.hash(), e2.hash());

    e2.set_property("flag", Value::Bool(true));
    assert_ne!(e1.hash(), e2.hash());
}

#[test]
fn generate_id_produces_unique_ids() {
    let a = generate_id();
    let b = generate_id();
    assert_ne!(a, b);
    assert_ne!(a, 0);
    assert_ne!(b, 0);
}

#[test]
fn task_comparator_orders_by_due_then_priority_then_start() {
    let mut t1 = Task::spawn("alpha", "owner", 1, 1, 10);
    t1.due_date = 25;
    t1.priority = 3;
    t1.scheduled_start = 15;

    let mut t2 = Task::spawn("beta", "owner", 1, 1, 10);
    t2.due_date = 25; // tie due date, use priority next
    t2.priority = 1;
    t2.scheduled_start = 5;

    let cmp = TaskComparator::default();
    // Due dates equal, priority decides
    assert_eq!(cmp.compare(&t1, &t2), Ordering::Greater);
    assert_eq!(cmp.compare(&t2, &t1), Ordering::Less);
}

#[test]
fn task_comparator_can_sort_by_title() {
    let mut cmp = TaskComparator::default();
    cmp.set_sort_field(0, SortField::TaskTitle);

    let mut t1 = Task::spawn("zulu", "owner", 1, 1, 10);
    let t2 = Task::spawn("alpha", "owner", 1, 1, 10);
    t1.properties
        .insert("title".into(), Value::Str("omega".into()));

    assert_eq!(cmp.compare(&t1, &t2), Ordering::Greater);
}

#[test]
fn netmsg_header_roundtrip() {
    let header = Header::new(MessageType::EntityInfo, true, 42);
    let bytes = header.to_bytes();
    let parsed = Header::from_bytes(&bytes).expect("header parse");
    assert_eq!(parsed.tag, data::netmsg::PACKET_HDR);
    assert_eq!(parsed.msg_type, MessageType::EntityInfo);
    assert!(parsed.is_response);
    assert_eq!(parsed.length, 42);
}

#[test]
fn netmsg_entity_roundtrip() {
    let msg = Message::EntityInfo(data::netmsg::EntityPayload {
        origin_id: 11,
        sync_id: 22,
        doid: 33,
        data: b"payload".to_vec(),
    });
    let buf = msg.to_bytes(MessageType::EntityInfo, true);
    let (header, parsed) = Message::from_bytes(&buf).unwrap();
    assert_eq!(header.msg_type, MessageType::EntityInfo);
    assert!(header.is_response);
    match parsed {
        Message::EntityInfo(e) => {
            assert_eq!(e.origin_id, 11);
            assert_eq!(e.sync_id, 22);
            assert_eq!(e.doid, 33);
            assert_eq!(e.data, b"payload".to_vec());
        }
        _ => panic!("unexpected message variant"),
    }
}
