extern crate jep106;

#[test]
fn print_all_test() {
    let nordic = jep106::get(2, 0x44);
    assert_eq!("Nordic VLSI ASA", nordic);
}