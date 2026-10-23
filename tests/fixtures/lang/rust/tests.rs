// SPDX-License-Identifier: AGPL-3.0-or-later
// Synthetic fixture: test functions and test-only modules.

pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

#[test]
fn top_level_test() {
    assert_eq!(add(1, 2), 3);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> u32 {
        2
    }

    #[test]
    fn adds() {
        assert_eq!(add(fixture(), 2), 4);
    }

    #[tokio::test]
    async fn adds_async() {
        assert!(add(0, 0) == 0);
    }
}
