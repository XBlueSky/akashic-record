#include <stdio.h>
#include "local.h"

int add(int a, int b) {
    return a + b;
}

static void helper(void) {
    printf("hello");
    add(1, 2);
}

int main(void) {
    helper();
    return 0;
}
