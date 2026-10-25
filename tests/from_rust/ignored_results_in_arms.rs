// A match arm that ignores a throwing call's result: the `val _ =` keeps its braces.
fn push(log: &mut Vec<String>, line: &str) -> Result<usize, String> {
    if line.is_empty() {
        return Err("empty".into());
    }
    log.push(line.to_string());
    Ok(log.len())
}

fn main() {
    let mut log = Vec::new();
    for c in ['a', '?', 'b'] {
        match c {
            '?' => {
                push(&mut log, "unknown").ok();
            }
            _ => {
                let _ = push(&mut log, &c.to_string());
            }
        }
    }
    let mut seen = Vec::new();
    match log.len() {
        3 => {
            seen.push(log.join(","));
        }
        _ => {}
    }
    println!("{:?}", seen);
}
