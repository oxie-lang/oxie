// A function returning the `Result` of a closure it calls on the spot.
use std::collections::HashMap;

fn install(manifest: &HashMap<&str, &str>) -> Result<String, String> {
    let installed = (|| {
        let id = manifest.get("id").ok_or("The extension has no id")?.to_string();
        let name = manifest.get("name").copied().unwrap_or(&id);
        if name.is_empty() {
            return Err(format!("{id} has an empty name"));
        }
        Ok(name.to_string())
    })();
    println!("cleaning up");
    installed
}

fn main() {
    let mut m = HashMap::new();
    println!("{:?}", install(&m));
    m.insert("id", "zig");
    println!("{:?}", install(&m));
    m.insert("name", "");
    println!("{:?}", install(&m));
}
