enum class Direction(val degrees: Int) {
    NORTH(0),
    EAST(90),
    SOUTH(180),
    WEST(270);

    fun describe(): String {
        return label()
    }

    private fun label(): String {
        return "dir"
    }
}
