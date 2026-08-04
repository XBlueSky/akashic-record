protocol Greeter {
    func greet() -> String
}

struct Person: Greeter {
    let name: String

    func greet() -> String {
        return format()
    }

    private func format() -> String {
        return "Hello, \(name)"
    }
}
