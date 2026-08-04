// EXT-6c-2: C# inheritance golden fixture
// Tests csharp_resolve_supertypes: class->interface (Implements) and class->class (Implements)
public interface IShape
{
    double Area();
}

public class Circle : IShape
{
    public double Area() { return 3.14; }
}

public class ColoredCircle : Circle
{
    public string Color() { return "red"; }
}
