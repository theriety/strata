pub fn unindexed_caller() -> u32 {
    local_value()
}

pub fn unindexed_module_caller() -> u32 {
    render::report()
}

pub fn unindexed_foreign_qualifier_caller() {
    let _ = std::io::Error::other("boom");
}

pub fn unindexed_crate_qualifier_caller() -> u32 {
    crate::Built::new()
}

pub fn unindexed_module_qualifier_caller() -> u32 {
    render::Frame::new()
}

pub fn unindexed_value_caller() -> Option<u32> {
    Some(1).map(double_value)
}
