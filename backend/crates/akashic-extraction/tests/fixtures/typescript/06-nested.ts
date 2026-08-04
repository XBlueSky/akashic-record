export function outer(items: number[]): number {
    const factor = 2;

    function scale(x: number): number {
        return x * factor;
    }

    function total(): number {
        return items.map(scale).reduce((a, b) => a + b, 0);
    }

    const doubler = (y: number) => y * 2;
    return total();
}

export function standalone(): number {
    return outer([1, 2, 3]);
}
