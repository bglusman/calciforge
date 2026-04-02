use hegel::generators::integers;
use hegel::TestCase;

#[hegel::test]
fn smoke_addition_commutative(tc: TestCase) {
    let x = tc.draw(integers::<i32>());
    let y = tc.draw(integers::<i32>());
    assert_eq!(x.wrapping_add(y), y.wrapping_add(x));
}
