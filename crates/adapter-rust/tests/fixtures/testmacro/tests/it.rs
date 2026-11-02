macro_rules! wrap {
    ($e:expr) => {
        #[test]
        fn generated() {
            $e;
        }
    };
}

wrap! { testmacro::helper() }

#[test]
fn plain() {}
