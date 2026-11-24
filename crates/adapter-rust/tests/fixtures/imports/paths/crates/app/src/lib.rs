mod a;
mod child;
mod t;
#[cfg(test)]
mod tests;

use child as kid;

pub struct Config;

pub fn parent() -> Config {
    child::run();
    kid::run();
    crate::a::f();
    Config
}

pub fn same_line() { a::f(); crate::a::g(); }

pub fn pick(mode: a::Mode) -> bool {
    match mode {
        a::Mode::On => true,
        a::Mode::Off => false,
    }
}

pub fn outside() {
    let _ = std::mem::size_of::<u8>();
    paths_other::shared();
    println!("{}", crate::t::shown());
}

pub fn own() {
    self::inner::here();
}

mod inner {
    pub fn here() {}
}

#[cfg(test)]
mod inline_tests {
    use crate::t::Tr;

    fn only() -> u8 {
        crate::t::shown()
    }
}
