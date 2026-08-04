# Basic build DAG.
all: build test

build: main.o utils.o
	$(CC) -o app main.o utils.o

test: build
	./run-tests.sh
