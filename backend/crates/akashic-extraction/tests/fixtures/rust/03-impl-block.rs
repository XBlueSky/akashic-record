struct Counter { n: u32 }

impl Counter {
    pub fn new() -> Self { Self { n: 0 } }
    pub fn increment(&mut self) { self.n += 1; }
}

impl std::fmt::Display for Counter {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.n)
    }
}
