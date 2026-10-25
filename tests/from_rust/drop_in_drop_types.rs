// `drop(x)` in a method of a type that implements `Drop` is std's `drop`.
use std::sync::Mutex;
struct View {
    term: Mutex<Vec<u32>>,
}
impl View {
    fn first(&self) -> Option<u32> {
        let term = self.term.lock().unwrap();
        let n = term.first().copied();
        drop(term);
        n
    }
}
impl Drop for View {
    fn drop(&mut self) {
        println!("dropped");
    }
}
fn main() {
    let v = View { term: Mutex::new(vec![3]) };
    println!("{:?}", v.first());
}
