#import "Local.h"
#import <Foundation/Foundation.h>

@interface Greeter : NSObject
- (void)sayHello;
@end

@implementation Greeter
- (void)sayHello {
    NSLog(@"hello");
}
@end
