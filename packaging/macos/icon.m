// Deterministic app icon; no downloaded artwork or image tooling dependencies.
#import <AppKit/AppKit.h>

int main(int argc, const char *argv[])
{
    @autoreleasepool {
        if (argc != 2) return 2;
        NSString *directory = @(argv[1]);
        for (NSNumber *number in @[@16, @32, @128, @256, @512]) {
            for (int scale = 1; scale <= 2; scale++) {
                const int size = number.intValue * scale;
                NSBitmapImageRep *bitmap = [[NSBitmapImageRep alloc]
                    initWithBitmapDataPlanes:NULL pixelsWide:size pixelsHigh:size
                    bitsPerSample:8 samplesPerPixel:4 hasAlpha:YES isPlanar:NO
                    colorSpaceName:NSDeviceRGBColorSpace bytesPerRow:0 bitsPerPixel:0];
                [NSGraphicsContext saveGraphicsState];
                [NSGraphicsContext setCurrentContext:[NSGraphicsContext graphicsContextWithBitmapImageRep:bitmap]];
                NSAffineTransform *transform = [NSAffineTransform transform];
                [transform scaleBy:size / 1024.0];
                [transform concat];
                NSBezierPath *tile = [NSBezierPath bezierPathWithRoundedRect:NSMakeRect(64, 64, 896, 896)
                    xRadius:208 yRadius:208];
                NSGradient *gradient = [[NSGradient alloc]
                    initWithStartingColor:[NSColor colorWithSRGBRed:0.12 green:0.19 blue:0.31 alpha:1]
                    endingColor:[NSColor colorWithSRGBRed:0.025 green:0.045 blue:0.09 alpha:1]];
                [gradient drawInBezierPath:tile angle:270];
                NSBezierPath *play = [NSBezierPath bezierPath];
                [play moveToPoint:NSMakePoint(395, 285)];
                [play lineToPoint:NSMakePoint(745, 512)];
                [play lineToPoint:NSMakePoint(395, 739)];
                [play closePath];
                [[NSColor colorWithSRGBRed:0.40 green:0.84 blue:1 alpha:1] setFill];
                [play fill];
                [NSGraphicsContext restoreGraphicsState];
                NSString *filename = [NSString stringWithFormat:@"icon_%dx%d%@.png", number.intValue,
                    number.intValue, scale == 2 ? @"@2x" : @""];
                NSData *png = [bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
                if (![png writeToFile:[directory stringByAppendingPathComponent:filename] atomically:YES]) return 1;
            }
        }
    }
    return 0;
}
