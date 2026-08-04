def outer(items: List[Int]): Int = {
  val factor = 2
  def scale(x: Int): Int = x * factor
  def total(): Int = items.map(scale).sum
  total()
}

def standalone(): Int = outer(List(1, 2, 3))
