fn out_of_line_support() -> u32 {
    super::helper()
}

#[test]
fn out_of_line_case() {
    assert_eq!(out_of_line_support(), 1);
}

fn unreferenced_support() {}
