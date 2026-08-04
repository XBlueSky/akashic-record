// Qualified Extends (stable_type_identifier) + generic Implements (generic_type).
class Repo extends data.Base with mutable.Cloneable[Repo] {
  def find(): Int = 0
}

// Bare Extends + qualified-generic Implements (generic over a stable base).
trait Service extends Greeter with util.Comparable[Service] {
  def serve(): String
}
