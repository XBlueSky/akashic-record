pub struct Widget {
    label: String,
}

pub enum Mode {
    On,
    Off,
}

pub type WidgetList = Vec<Widget>;

pub fn build(mode: Mode, items: WidgetList) -> Widget {
    let _ = mode;
    let _ = items;
    Widget { label: String::new() }
}
