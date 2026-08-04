namespace outer {
    namespace inner {
        void deeply_nested() {}
    }
    void outer_fn() {
        inner::deeply_nested();
    }
}
