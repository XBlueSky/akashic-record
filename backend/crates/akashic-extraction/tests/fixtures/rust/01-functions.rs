pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

async fn fetch_data() -> Result<String, std::io::Error> {
    Ok("data".to_string())
}

fn private_helper() {
    println!("internal");
}
