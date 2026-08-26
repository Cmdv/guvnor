use guvnor::engine::plan::new_session_id;

#[test]
fn session_id_is_uuid_v4_shaped() {
    let id = new_session_id().unwrap();
    assert_eq!(id.len(), 36);
    let b = id.as_bytes();
    assert!(b[8] == b'-' && b[13] == b'-' && b[18] == b'-' && b[23] == b'-');
    assert_eq!(&id[14..15], "4"); // version 4 nibble
    assert!(matches!(&id[19..20], "8" | "9" | "a" | "b")); // variant 10xx
    assert_ne!(new_session_id().unwrap(), id); // not constant
}
