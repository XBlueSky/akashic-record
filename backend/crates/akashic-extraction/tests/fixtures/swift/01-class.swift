class Counter {
    var count: Int
    private var step: Int

    init(start: Int) {
        self.count = start
        self.step = 1
    }

    public func increment() {
        self.bump()
    }

    private func bump() {
        count += step
    }
}
