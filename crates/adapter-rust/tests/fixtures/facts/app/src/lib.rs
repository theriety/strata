pub mod widget;

pub fn main() {}

#[cfg(test)]
#[path = "support/check.rs"]
mod check;

#[cfg(test)]
mod inline {
    #[test]
    fn same_file() {}
}
