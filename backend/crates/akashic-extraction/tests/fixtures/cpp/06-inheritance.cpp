// EXT-6c-2: C++ inheritance golden fixture
// Tests cpp_resolve_supertypes: class -> base class (Extends)
class Shape {
public:
    virtual double area() = 0;
};

class Circle : public Shape {
public:
    double area() override { return 3.14; }
};
