pub trait Animal {
    fn name(&self) -> String;
    fn sound(&self) -> String {
        "...".to_string()
    }
}
