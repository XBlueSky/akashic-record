package model

type User struct {
	ID    uint64
	Name  string
	Email string
}

type Admin struct {
	User
	Level int
}
