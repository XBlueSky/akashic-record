# Target-specific variable assignments (`target: VAR = val`) must be treated as
# variables scoped to the target, NOT as a rule with `VAR`/`=`/`val` as
# prerequisites. Only the real rules below should contribute prerequisite edges.
build: CFLAGS = -O2
build: CFLAGS += -Wall
debug: CFLAGS := -g

build: main.o
	$(CC) $(CFLAGS) -o app main.o

debug: build
	./app --debug
