static int square(int x) {
    return x * x;
}

int sumOfSquares(int a, int b) {
    return square(a) + square(b);
}

@interface Calculator : NSObject
- (int)compute;
@end

@implementation Calculator
- (int)compute {
    return sumOfSquares(3, 4);
}
@end
