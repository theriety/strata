pub mod widget;

#[cfg(test)]
#[path = "support/check.rs"]
mod check;

#[cfg(test)]
mod inline {
    #[test]
    fn same_file() {}
}
