// SPDX-License-Identifier: AGPL-3.0-or-later
// Synthetic fixture: the ways code refers to other code.

fn run(input: Config) -> Result<Output, Error> {
    let parsed = parse(input);
    let value = crate::util::normalize(parsed);
    let total = value.sum();
    let typed = convert::<u8>(total);
    let shape = Shape::Circle(1.0);
    let point = Point { x: 1, y: 2 };
    log!("done {}", format_total(total));
    let closure = |n: u32| helper(n);
    Ok(Output::new(typed))
}
