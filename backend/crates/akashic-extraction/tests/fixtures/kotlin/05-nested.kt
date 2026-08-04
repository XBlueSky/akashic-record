fun outer(items: List<Int>): Int {
    val factor = 2
    fun scale(x: Int): Int {
        return x * factor
    }
    fun total(): Int {
        return items.map { scale(it) }.sum()
    }
    return total()
}

fun standalone(): Int {
    return outer(listOf(1, 2, 3))
}
