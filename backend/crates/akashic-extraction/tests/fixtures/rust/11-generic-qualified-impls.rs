pub struct Wrapper(u32);

pub struct Holder<T>(T);

// Generic trait impl: trait name is a `generic_type` (`From<u32>` -> `From`).
impl From<u32> for Wrapper {
    fn from(v: u32) -> Self {
        Wrapper(v)
    }
}

// Path-qualified trait impl: trait name is a `scoped_type_identifier`
// (`std::fmt::Display` -> `Display`).
impl std::fmt::Display for Wrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// Generic-param impl: `<T>` (a `type_parameters` node before `for`) must NOT
// be emitted as a supertype; only `Clone` is.
impl<T: Clone> Clone for Holder<T> {
    fn clone(&self) -> Self {
        Holder(self.0.clone())
    }
}

// Supertrait bounds: a path-qualified bound (`std::fmt::Debug` -> `Debug`) and
// a generic bound (`From<u8>` -> `From`) both yield IMPLEMENTS edges.
pub trait Loud: std::fmt::Debug + From<u8> {}
