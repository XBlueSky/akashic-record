type Id = string

type Point = {
  x: number,
  y: number,
}

local V = {}

function V.make(id: Id): Point
  return build(id)
end

function V:translate(p: Point, dx: number): Point
  return p
end

return V
