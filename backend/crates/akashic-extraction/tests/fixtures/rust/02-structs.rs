pub struct User {
    pub id: u64,
    name: String,
}

#[derive(Debug, Clone)]
struct Config {
    port: u16,
}
