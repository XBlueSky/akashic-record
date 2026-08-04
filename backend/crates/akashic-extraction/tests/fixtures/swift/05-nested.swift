func outer(_ items: [Int]) -> Int {
    let factor = 2
    func scale(_ x: Int) -> Int {
        return x * factor
    }
    func total() -> Int {
        return items.map(scale).reduce(0, +)
    }
    return total()
}

func standalone() -> Int {
    return outer([1, 2, 3])
}
