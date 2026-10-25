// A function returning a `Result` that ends in a `loop` it only leaves by returning.
fn first_even(items: &[i32]) -> Result<i32, String> {
    let mut i = 0;
    loop {
        match items.get(i) {
            Some(n) if n % 2 == 0 => return Ok(*n),
            Some(_) => {}
            None => return Err(format!("no even number in {} items", items.len())),
        }
        i += 1;
    }
}

fn main() {
    println!("{:?} {:?}", first_even(&[1, 3, 4]), first_even(&[1]));
}
