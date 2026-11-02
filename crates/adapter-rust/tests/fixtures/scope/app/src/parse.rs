mod refs;
#[cfg(test)]
mod tests;
#[cfg(not(test))]
mod prod_only;
#[cfg(all(test, feature = "x"))]
mod gated;

pub fn go() {
    refs::helper();
}
