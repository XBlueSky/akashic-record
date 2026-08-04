.PHONY: clean

%.o: %.c
	$(CC) -c $< -o $@

clean:
	rm -f *.o
