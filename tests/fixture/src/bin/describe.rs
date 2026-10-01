fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    println!("{}", fixture::describe(&arg));
}
