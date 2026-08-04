macro_rules! repeat {
    ($n:expr, $body:block) => {
        for _ in 0..$n { $body }
    };
}

fn use_macros() {
    println!("hello");
    repeat!(3, { print!("."); });
}
