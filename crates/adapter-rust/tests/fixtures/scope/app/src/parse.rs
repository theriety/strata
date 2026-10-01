mod refs;
#[cfg(test)]
mod tests;
#[cfg(all(test, not(windows)))]
mod extra_tests;
#[cfg(all(test, feature = "x"))]
mod gated;

pub fn go() {
    refs::helper();
}
