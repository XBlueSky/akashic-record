struct Point {
    int x;
    int y;
};

typedef struct {
    char name[64];
    int age;
} Person;

void print_point(struct Point p) {
    printf("%d,%d", p.x, p.y);
}
