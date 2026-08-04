package shape

type Shape interface {
	Area() float64
	Perimeter() float64
}

type Drawer interface {
	Draw()
}
