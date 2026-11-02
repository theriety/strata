pub fn first() {}

pub(crate) fn helper() {}

#[cfg(test)]
mod tests {
    use super::helper;

    macro_rules! wrap {
        ($e:expr) => {
            #[test]
            fn generated() {
                $e;
            }
        };
    }

    wrap! { helper() }
}
