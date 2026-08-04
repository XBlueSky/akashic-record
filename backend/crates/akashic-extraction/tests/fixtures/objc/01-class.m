@interface Counter : NSObject
- (void)increment;
- (void)setValue:(int)v andLabel:(NSString *)l;
@end

@implementation Counter
- (void)increment {
    self.count += 1;
}

- (void)setValue:(int)v andLabel:(NSString *)l {
    [self increment];
    self.value = v;
    self.label = l;
}
@end
