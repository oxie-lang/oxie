// Attributes on parameters, arguments, elements, closures and `match` bodies.
fn scale(n: i32, #[cfg(debug_assertions)] factor: i32, offset: i32) -> i32 {
    #[cfg(debug_assertions)]
    return n * factor + offset;
    #[cfg(not(debug_assertions))]
    {
        n + offset
    }
}

fn kind(x: Option<i32>) -> &'static str {
    match x {
        #![allow(unused_variables)]
        Some(v) => "some",
        None => "none",
    }
}

fn main() {
    let a = scale(
        3,
        #[cfg(debug_assertions)]
        100,
        4,
    );
    let v = vec![1, #[cfg(test)] 2, 3];
    let total = v.iter().fold(0, #[inline(always)] |acc, x| acc + x);
    println!("{} {:?} {} {} {}", a, v, total, kind(Some(1)), kind(None));
}
