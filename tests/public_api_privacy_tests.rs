#[test]
fn websocket_binary_wire_internals_are_not_public_api() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/ui/websocket_binary_wire_internals_private.rs");
}
