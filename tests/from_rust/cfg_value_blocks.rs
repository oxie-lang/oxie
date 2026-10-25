// A function whose value is one of two blocks chosen by `#[cfg]` stays Rust.
fn platform() -> Result<String, String> {
    #[cfg(unix)]
    {
        let name = "unix".to_string();
        Ok(name)
    }
    #[cfg(not(unix))]
    {
        Err("not unix".to_string())
    }
}

fn main() {
    println!("{:?}", platform().is_ok() || cfg!(not(unix)));
}
