// SPDX-License-Identifier: AGPL-3.0-or-later
// Synthetic fixture: every kind of Rust declaration the adapter extracts.

pub fn public_fn(a: u32) -> u32 {
    a
}

pub(crate) fn crate_fn() {}

fn private_fn() {}

pub struct Point {
    pub x: i64,
    y: i64,
}

pub enum Shape {
    Circle(f64),
    Square { side: f64 },
}

union Bits {
    int: u32,
    float: f32,
}

pub trait Area: Clone + std::fmt::Debug {
    fn area(&self) -> f64;

    fn describe(&self) -> String {
        String::new()
    }
}

impl Area for Point {
    fn area(&self) -> f64 {
        0.0
    }
}

impl std::fmt::Display for Point {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}

impl std::fmt::Debug for Point {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}

impl<T> Wrapper<T> {
    pub fn new(value: T) -> Self {
        Wrapper(value)
    }
}

pub const LIMIT: usize = 10;
static mut COUNTER: u32 = 0;
pub type Id = u64;

macro_rules! square {
    ($x:expr) => {
        $x * $x
    };
}

mod inner {
    pub fn nested() {
        fn helper() {}
    }
}

mod external;

#[path = "elsewhere.rs"]
pub mod moved;
