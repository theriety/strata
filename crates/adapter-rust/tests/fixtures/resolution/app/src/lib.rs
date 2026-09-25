mod omitted;
mod render;

pub fn external_caller() -> u32 {
    resolution_external::external_value() + local_value()
}

pub fn unresolved_caller(value: UnknownReceiver) -> u32 {
    value.first() + local_value()
}

pub fn macro_caller(value: UnknownReceiver) {
    assert_eq!(value.first(), local_value());
}

pub fn omitted_caller() -> u32 {
    omitted::omitted_value() + local_value()
}

pub fn local_caller(value: Local) -> u32 {
    local_value() + value.measure()
}

pub fn local_value() -> u32 {
    1
}

pub struct Local;

impl Local {
    pub fn measure(&self) -> u32 {
        2
    }
}

pub struct Built;

impl Built {
    pub fn new() -> u32 {
        3
    }
}

// Shares its name with `std::io::Error`: a foreign-rooted path must not bind here.
pub struct Error;

impl Error {
    pub fn other() -> u32 {
        6
    }
}

pub fn associated_caller() -> u32 {
    Built::new()
}

pub fn module_caller() -> u32 {
    render::report()
}

// Shares the module's name: a value and a module live in different namespaces.
pub fn render() -> u32 {
    4
}

mod collisions {
    pub fn external_value() -> u32 {
        99
    }
    pub fn first() -> u32 {
        99
    }
    pub fn omitted_value() -> u32 {
        99
    }
}
