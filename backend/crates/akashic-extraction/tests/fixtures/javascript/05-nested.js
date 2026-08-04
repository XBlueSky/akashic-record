export function outer(items) {
    const factor = 2;

    function scale(x) {
        return x * factor;
    }

    function total() {
        return items.map(scale).reduce((a, b) => a + b, 0);
    }

    const doubler = (y) => y * 2;
    return total();
}

export function standalone() {
    return outer([1, 2, 3]);
}
