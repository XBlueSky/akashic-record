// EXT: C++ templated-base inheritance golden fixture
// Tests cpp_resolve_supertypes: a derived class whose base class is a
// template_type (`class D : public Base<T>`) must still emit an EXTENDS
// edge to the bare base name (`Base`).
template <typename T>
class Base {
public:
    T value;
};

class Derived : public Base<int> {
public:
    int extra;
};
