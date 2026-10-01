use std::process::Command;

// Runs the library only through a subprocess, so its coverage proves that
// child processes write into the parent test's profile.
#[test]
fn cli_describes_a_negative_number() {
    let out = Command::new(env!("CARGO_BIN_EXE_describe"))
        .arg("-3")
        .output()
        .expect("run the describe binary");
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "-3 is negative");
}
