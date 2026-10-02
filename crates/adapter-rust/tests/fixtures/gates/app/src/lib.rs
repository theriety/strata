pub struct Helper;

impl Helper {
    pub fn run(&self) {}
}

pub fn shared() {}

#[cfg(all(test, not(windows)))]
mod all_tests;
#[cfg(any(test, unix))]
mod any_mixed;
#[cfg(test)]
#[path = "support/cases.rs"]
mod cases;
#[cfg(not(test))]
mod real;
