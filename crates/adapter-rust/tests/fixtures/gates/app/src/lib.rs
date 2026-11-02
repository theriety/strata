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

#[cfg(not(test))]
pub fn inline_drive(h: &Helper) {
    h.run();
    // shares its name with a cfg(test) helper, which must not count.
    h.path_support();
}
