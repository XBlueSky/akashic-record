/// Outer function with two named nested helpers.
pub fn outer(items: &[i32]) -> i32 {
    let factor = 2;

    fn scale(x: i32, factor: i32) -> i32 {
        x * factor
    }

    fn total(items: &[i32], factor: i32) -> i32 {
        items.iter().map(|&i| scale(i, factor)).sum()
    }

    total(items, factor)
}

/// Top-level function that calls outer().
pub fn standalone() -> i32 {
    outer(&[1, 2, 3])
}
