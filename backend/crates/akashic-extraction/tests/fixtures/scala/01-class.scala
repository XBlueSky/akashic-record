class Counter(start: Int) {
  private var count: Int = start

  def increment(): Int = {
    count = count + 1
    count
  }

  def incrementTwice(): Int = {
    increment()
    increment()
  }

  protected def reset(): Unit = {
    count = 0
  }
}
