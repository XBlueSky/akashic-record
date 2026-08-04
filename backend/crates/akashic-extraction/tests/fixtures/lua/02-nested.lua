local function make_counter(start)
  local count = start
  local function increment()
    count = count + 1
    return count
  end
  return increment
end

function outer()
  local inner = function()
    return compute()
  end
  return inner
end
