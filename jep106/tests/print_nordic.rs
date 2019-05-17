extern crate jep106;

#[test]
fn print_nordic_test() {
    let nordic = jep106::JEP106Code::new(0x02, 0x44).get();
    assert_eq!("Nordic VLSI ASA", nordic);
}