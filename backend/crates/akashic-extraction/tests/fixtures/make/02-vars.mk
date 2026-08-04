# Variable assignments mixed with rules.
CC := gcc
CFLAGS = -O2
OBJ ?= x.o
LDFLAGS += -lm

app: $(OBJ)
	$(CC) $(CFLAGS) -o app $(OBJ)

clean:
	rm -f app *.o
