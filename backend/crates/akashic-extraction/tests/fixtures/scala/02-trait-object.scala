trait Greeter {
  def name: String

  def greet(): String = {
    "Hello, " + name
  }
}

object EnglishGreeter extends Greeter {
  def name: String = "World"

  def shout(): String = {
    greet()
  }
}
