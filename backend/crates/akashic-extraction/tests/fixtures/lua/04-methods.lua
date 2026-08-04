local T = {}

function T.create(name)
  return name
end

function T:render()
  return self.name
end

function T.Util.format(s)
  return s
end

return T
