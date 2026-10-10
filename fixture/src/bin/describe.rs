fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    println!("{}", dwd_fixture::describe(&arg));
}
