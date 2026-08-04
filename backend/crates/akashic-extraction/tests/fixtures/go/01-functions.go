package main

import (
	"fmt"
	"strings"
)

func Add(a, b int) int {
	return a + b
}

func greet(name string) {
	fmt.Println(strings.ToUpper(name))
}
