template <typename T>
class Container {
public:
    void push(T value) {
        items_.push_back(value);
    }
    T pop();

private:
    std::vector<T> items_;
};
