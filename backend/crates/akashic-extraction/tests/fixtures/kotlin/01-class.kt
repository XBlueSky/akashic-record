class Counter(private val start: Int) {
    var count: Int = 0
    private val label: String = "counter"

    constructor() : this(0)

    fun increment() {
        count = count + 1
        bump()
    }

    private fun bump() {
        count = count + 1
    }

    internal fun reset() {
        count = start
    }
}
