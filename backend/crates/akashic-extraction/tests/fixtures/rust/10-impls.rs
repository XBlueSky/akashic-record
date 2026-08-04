pub trait Draw {
    fn draw(&self);
}

pub struct Button {
    label: String,
}

impl Draw for Button {
    fn draw(&self) {
        let _ = &self.label;
    }
}
