// An Option turned into an error at the end of a function: `x.ok_or_else(|| "..".into())`.
use std::collections::HashMap;

fn port(config: &HashMap<&str, &str>) -> Result<u16, String> {
    let text = config.get("port").ok_or("no port")?;
    text.parse::<u16>().ok().ok_or_else(|| format!("bad port {text}"))
}

fn host(config: &HashMap<&str, &str>) -> Result<String, String> {
    config.get("host").map(|h| h.to_string()).ok_or_else(|| "no host".into())
}

fn main() {
    let mut config = HashMap::new();
    println!("{:?} {:?}", port(&config), host(&config));
    config.insert("port", "80x");
    config.insert("host", "example.org");
    println!("{:?} {:?}", port(&config), host(&config));
    config.insert("port", "8080");
    println!("{:?}", port(&config));
}
