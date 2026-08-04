@protocol Drawable <NSObject>
- (void)draw;
- (void)resizeTo:(int)width height:(int)height;
@end

@interface Shape : NSObject <Drawable>
- (void)draw;
@end

@implementation Shape
- (void)draw {
    [self resizeTo:10 height:20];
}
@end
