pub(super) fn run() -> u32 {
    3
}

#[cfg(test)]
mod tests {
    use crate::b::shared;

    #[test]
    fn inline_case() {
        let _ = shared();
    }
}
