pub(crate) fn host() {}

cfg_if::cfg_if! {
    if #[cfg(feature = "x")] {
        fn f() {
            crate::a::H.go();
        }
    }
}

pub(crate) fn inner_gated() {
    #![cfg(feature = "x")]
    crate::a::via_attr_target();
}
