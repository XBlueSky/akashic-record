enum Direction {
    case north
    case south
    case east
    case west

    func opposite() -> Direction {
        switch self {
        case .north: return .south
        case .south: return .north
        case .east: return .west
        case .west: return .east
        }
    }

    func describe() -> String {
        return label()
    }

    private func label() -> String {
        return "direction"
    }
}
