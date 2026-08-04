package geom

type Point struct {
	X, Y float64
}

func (p Point) Distance(q Point) float64 {
	return 0.0
}

func (p *Point) Translate(dx, dy float64) {
	p.X += dx
	p.Y += dy
}
