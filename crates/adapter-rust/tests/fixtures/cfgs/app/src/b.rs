pub(crate) fn go() {
    #[cfg(feature = "x")]
    crate::a::helper();
}

pub(crate) fn calm() {
    crate::a::plain();
}

pub(crate) struct S {
    #[cfg(windows)]
    field: crate::a::T,
}

pub(crate) struct V {
    field: crate::a::U,
}
