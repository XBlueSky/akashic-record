namespace MyApp.Contracts
{
    /// <summary>Defines a shape with area computation.</summary>
    public interface IShape
    {
        double Area();
        double Perimeter();
        string Describe();
    }

    /// <summary>An interface for serializable objects.</summary>
    public interface ISerializable
    {
        string Serialize();
    }
}
