// `return` of an `if` whose branches are `Ok(..)` and `Err(..)`, with errors made by `.into()`.
fn kinds(names: &[&str]) -> Result<Option<usize>, String> {
    let Some(at) = names.iter().position(|n| *n == "name") else {
        return if names.is_empty() { Ok(None) } else { Err("the query has no @name".into()) };
    };
    if at > 3 {
        return if at > 5 { Err(format!("@name at {at}").into()) } else if at == 4 { Ok(Some(40)) } else { Ok(None) };
    }
    Ok(Some(at))
}

fn main() {
    println!("{:?}", kinds(&[]));
    println!("{:?}", kinds(&["a"]));
    println!("{:?}", kinds(&["a", "name"]));
    println!("{:?}", kinds(&["a", "b", "c", "d", "name"]));
    println!("{:?}", kinds(&["a", "b", "c", "d", "e", "f", "name"]));
}
