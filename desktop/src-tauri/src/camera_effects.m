#import <Foundation/Foundation.h>
#import <Vision/Vision.h>
#import <CoreImage/CoreImage.h>

static const NSUInteger ClipyFaceProbeInterval = 12;

@interface ClipyFaceState : NSObject
@property CGRect bounds;
@property CGRect face;
@property NSUInteger frame;
@property NSUInteger misses;
@property(strong) VNDetectFaceRectanglesRequest *request;
@end

@implementation ClipyFaceState
- (instancetype)init {
    self = [super init];
    if (self) {
        _face = CGRectNull;
        _request = [VNDetectFaceRectanglesRequest new];
    }
    return self;
}
@end

void *clipy_camera_face_state_create(void) {
    return (__bridge_retained void *)[ClipyFaceState new];
}

void clipy_camera_face_state_destroy(void *state) {
    (void)(__bridge_transfer ClipyFaceState *)state;
}

static CGRect ClipyClampRect(CGRect rect, CGRect bounds) {
    CGRect clamped = CGRectIntersection(rect, bounds);
    return CGRectIsNull(clamped) || CGRectIsEmpty(clamped) ? CGRectNull : clamped;
}

// Vision is deliberately sampled rather than run on every camera frame. The
// last face box is owned by the processor and eased toward new readings
// so neither the touch-up mask nor the exposure meter visibly jumps.
static CGRect ClipyFaceRect(ClipyFaceState *state, CIImage *source, CGRect bounds, BOOL forceProbe) {
    if (!CGRectEqualToRect(state.bounds, bounds)) {
        state.bounds = bounds;
        state.face = CGRectNull;
        state.frame = 0;
        state.misses = 0;
    }
    NSUInteger frame = state.frame;
    state.frame = frame + 1;
    CGRect cached = ClipyClampRect(state.face, bounds);
    if (!forceProbe && frame % ClipyFaceProbeInterval != 0) return cached;

    VNDetectFaceRectanglesRequest *request = state.request;
    CGFloat scale = MIN(1.0, 480.0 / MAX(bounds.size.width, bounds.size.height));
    CIImage *analysis = [source imageByApplyingTransform:CGAffineTransformMakeScale(scale, scale)];
    VNImageRequestHandler *handler = [[VNImageRequestHandler alloc] initWithCIImage:analysis options:@{}];
    NSError *error = nil;
    if (![handler performRequests:@[request] error:&error] || request.results.count == 0) {
        state.misses += 1;
        if (state.misses >= 2) {
            state.face = CGRectNull;
            return CGRectNull;
        }
        return cached;
    }
    state.misses = 0;

    VNFaceObservation *largest = request.results.firstObject;
    for (VNFaceObservation *candidate in request.results) {
        CGFloat candidateArea = candidate.boundingBox.size.width * candidate.boundingBox.size.height;
        CGFloat largestArea = largest.boundingBox.size.width * largest.boundingBox.size.height;
        if (candidateArea > largestArea) largest = candidate;
    }
    CGRect normalized = largest.boundingBox;
    CGRect detected = CGRectMake(
        bounds.origin.x + normalized.origin.x * bounds.size.width,
        bounds.origin.y + normalized.origin.y * bounds.size.height,
        normalized.size.width * bounds.size.width,
        normalized.size.height * bounds.size.height
    );
    detected = ClipyClampRect(detected, bounds);
    if (CGRectIsNull(detected)) {
        state.face = CGRectNull;
        return CGRectNull;
    }
    if (!CGRectIsNull(cached)) {
        const CGFloat oldWeight = 0.72;
        const CGFloat newWeight = 1.0 - oldWeight;
        detected = CGRectMake(
            cached.origin.x * oldWeight + detected.origin.x * newWeight,
            cached.origin.y * oldWeight + detected.origin.y * newWeight,
            cached.size.width * oldWeight + detected.size.width * newWeight,
            cached.size.height * oldWeight + detected.size.height * newWeight
        );
    }
    detected = ClipyClampRect(detected, bounds);
    state.face = detected;
    return detected;
}

static CIImage *ClipyFaceMask(CGRect face, CGRect bounds) {
    // A face rectangle is tighter than the skin users expect the control to
    // cover. Expand it slightly, then feather an ellipse so hair, glasses,
    // eyes, and the room do not acquire a rectangular blur boundary.
    face = CGRectInset(face, -face.size.width * 0.10, -face.size.height * 0.12);
    face = ClipyClampRect(face, bounds);
    if (CGRectIsNull(face)) return nil;
    CGFloat base = MIN(face.size.width, face.size.height);
    CIFilter *gradient = [CIFilter filterWithName:@"CIRadialGradient" withInputParameters:@{
        @"inputCenter": [CIVector vectorWithX:0 Y:0],
        @"inputRadius0": @(base * 0.32),
        @"inputRadius1": @(base * 0.51),
        @"inputColor0": [CIColor colorWithRed:1 green:1 blue:1 alpha:1],
        @"inputColor1": [CIColor colorWithRed:0 green:0 blue:0 alpha:1],
    }];
    CGFloat scaleX = face.size.width / base;
    CGFloat scaleY = face.size.height / base;
    CGAffineTransform ellipse = CGAffineTransformMake(
        scaleX, 0, 0, scaleY, CGRectGetMidX(face), CGRectGetMidY(face)
    );
    return [[gradient.outputImage imageByApplyingTransform:ellipse] imageByCroppingToRect:bounds];
}

// `touchUp` is 0 (off) to 1; `exposureGain` is the brightness multiplier to
// apply (1 = none); `lumaOut`, when non-NULL, receives the face-weighted
// luminance (0..1) before exposure so the caller can ease the next gain.
int clipy_camera_effect(void *faceState, const unsigned char *input, size_t width, size_t height,
                        size_t stride, unsigned char *output, size_t outputStride,
                        int mode, int canvas, int frame, unsigned int canvasRgb, bool opaque,
                        const unsigned char *backgroundPixels, size_t backgroundWidth, size_t backgroundHeight,
                        float touchUp, float exposureGain, float *lumaOut) {
    @autoreleasepool {
        if (!faceState) return 2;
        CGColorSpaceRef colorSpace = NULL;
        @try {
            static CIContext *context;
            static dispatch_once_t once;
            dispatch_once(&once, ^{ context = [CIContext contextWithOptions:@{kCIContextCacheIntermediates: @NO}]; });
            colorSpace = CGColorSpaceCreateDeviceRGB();
            NSData *bytes = [NSData dataWithBytesNoCopy:(void *)input length:stride * height freeWhenDone:NO];
            CGRect bounds = CGRectMake(0, 0, width, height);
            CIImage *source = [CIImage imageWithBitmapData:bytes bytesPerRow:stride
                size:bounds.size format:kCIFormatRGBA8 colorSpace:colorSpace];
            CIImage *result = source;
            CGRect face = CGRectNull;
            if (touchUp > 0.0f || lumaOut != NULL) {
                face = ClipyFaceRect((__bridge ClipyFaceState *)faceState, source, bounds, lumaOut != NULL);
            }
            if (mode != 0) {
                NSMutableDictionary *local = NSThread.currentThread.threadDictionary;
                VNGeneratePersonSegmentationRequest *request = local[@"clipy.personSegmentation"];
                if (!request) {
                    request = [VNGeneratePersonSegmentationRequest new];
                    request.qualityLevel = VNGeneratePersonSegmentationRequestQualityLevelBalanced;
                    request.outputPixelFormat = kCVPixelFormatType_OneComponent8;
                    local[@"clipy.personSegmentation"] = request;
                }
                CGFloat scale = MIN(1.0, 640.0 / MAX(width, height));
                CIImage *analysis = [source imageByApplyingTransform:CGAffineTransformMakeScale(scale, scale)];
                VNImageRequestHandler *handler = [[VNImageRequestHandler alloc] initWithCIImage:analysis options:@{}];
                NSError *error = nil;
                if (![handler performRequests:@[request] error:&error] || request.results.count == 0) {
                    return 1;
                }
                CIImage *mask = [CIImage imageWithCVPixelBuffer:request.results.firstObject.pixelBuffer];
                mask = [mask imageByApplyingTransform:CGAffineTransformMakeScale(width / mask.extent.size.width, height / mask.extent.size.height)];
                CIImage *background;
                if (mode == 1) {
                    background = [[[source imageByClampingToExtent]
                        imageByApplyingFilter:@"CIGaussianBlur" withInputParameters:@{kCIInputRadiusKey: @(MAX(width, height) * 0.075)}]
                        imageByCroppingToRect:bounds];
                } else {
                    CIColor *color = [CIColor colorWithRed:0 green:0 blue:0 alpha:0];
                    if (canvas == 1 || (canvas == 0 && opaque)) color = [CIColor colorWithRed:0.039 green:0.051 blue:0.086 alpha:1];
                    if (canvas == 2) color = [CIColor colorWithRed:0.980 green:0.965 blue:0.941 alpha:1];
                    // Solid colour mat: 0xRRGGBB packed by the Rust side.
                    if (canvas == 8) color = [CIColor colorWithRed:((canvasRgb >> 16) & 0xff) / 255.0
                                                             green:((canvasRgb >> 8) & 0xff) / 255.0
                                                              blue:(canvasRgb & 0xff) / 255.0 alpha:1];
                    background = [[CIImage imageWithColor:color] imageByCroppingToRect:bounds];
                    if (canvas == 3) {
                        background = [[CIFilter filterWithName:@"CILinearGradient" withInputParameters:@{
                            @"inputPoint0": [CIVector vectorWithX:0 Y:0], @"inputPoint1": [CIVector vectorWithX:width Y:height],
                            @"inputColor0": [CIColor colorWithRed:1 green:0.30 blue:0.16],
                            @"inputColor1": [CIColor colorWithRed:1 green:0.69 blue:0.13]
                        }].outputImage imageByCroppingToRect:bounds];
                    }
                }
                if (mode == 2 && backgroundPixels && backgroundWidth && backgroundHeight) {
                    NSData *pixels = [NSData dataWithBytesNoCopy:(void *)backgroundPixels length:backgroundWidth * backgroundHeight * 4 freeWhenDone:NO];
                    CIImage *custom = [CIImage imageWithBitmapData:pixels bytesPerRow:backgroundWidth * 4 size:CGSizeMake(backgroundWidth, backgroundHeight) format:kCIFormatRGBA8 colorSpace:colorSpace];
                    CGFloat factor = MAX((CGFloat)width / backgroundWidth, (CGFloat)height / backgroundHeight);
                    custom = [custom imageByApplyingTransform:CGAffineTransformMakeScale(factor, factor)];
                    custom = [custom imageByApplyingTransform:CGAffineTransformMakeTranslation((width - custom.extent.size.width) / 2, (height - custom.extent.size.height) / 2)];
                    background = [custom imageByCroppingToRect:bounds];
                }
                result = [source imageByApplyingFilter:@"CIBlendWithMask" withInputParameters:@{
                    kCIInputBackgroundImageKey: background, kCIInputMaskImageKey: mask
                }];
            }
            if (touchUp > 0.0f && !CGRectIsNull(face)) {
                // Noise reduction preserves strong edges far better than a
                // Gaussian blur. It is still blended only partway and only
                // over the face so maximum strength keeps glasses readable.
                CIImage *denoised = [result imageByApplyingFilter:@"CINoiseReduction"
                    withInputParameters:@{
                        @"inputNoiseLevel": @(0.012 + 0.010 * touchUp),
                        @"inputSharpness": @(0.70 + 0.25 * touchUp),
                    }];
                CIImage *polished = [result imageByApplyingFilter:@"CIDissolveTransition"
                    withInputParameters:@{
                        kCIInputTargetImageKey: denoised,
                        kCIInputTimeKey: @(0.15 + 0.35 * touchUp),
                    }];
                CIImage *faceMask = ClipyFaceMask(face, bounds);
                if (faceMask) {
                    result = [polished imageByApplyingFilter:@"CIBlendWithMask"
                        withInputParameters:@{
                            kCIInputBackgroundImageKey: result,
                            kCIInputMaskImageKey: faceMask,
                        }];
                }
            }
            if (lumaOut != NULL) {
                // Meter the face instead of the full room. A bright wall was
                // previously interpreted as an overexposed subject, so Auto
                // lighting darkened the person who needed the lift. When no
                // face is available, the central portrait-weighted crop is a
                // safer fallback than the frame edges.
                CGRect meter = face;
                if (CGRectIsNull(meter)) {
                    meter = CGRectMake(
                        bounds.origin.x + bounds.size.width * 0.23,
                        bounds.origin.y + bounds.size.height * 0.16,
                        bounds.size.width * 0.54,
                        bounds.size.height * 0.68
                    );
                } else {
                    meter = CGRectInset(meter, meter.size.width * 0.12, meter.size.height * 0.10);
                }
                meter = ClipyClampRect(meter, bounds);
                CIImage *average = [source imageByApplyingFilter:@"CIAreaAverage"
                    withInputParameters:@{kCIInputExtentKey: [CIVector vectorWithCGRect:meter]}];
                unsigned char pixel[4] = {0, 0, 0, 0};
                [context render:average toBitmap:pixel rowBytes:4 bounds:CGRectMake(0, 0, 1, 1) format:kCIFormatRGBA8 colorSpace:colorSpace];
                *lumaOut = (0.2126f * pixel[0] + 0.7152f * pixel[1] + 0.0722f * pixel[2]) / 255.0f;
            }
            if (fabsf(exposureGain - 1.0f) >= 0.01f) {
                if (exposureGain > 1.0f) {
                    // Lift facial shadows while protecting the wall and other
                    // highlights, then use only part of the requested global
                    // exposure change. This reads like better lighting rather
                    // than a brightness overlay.
                    float lift = MIN(0.8f, (exposureGain - 1.0f) * 1.6f);
                    result = [result imageByApplyingFilter:@"CIHighlightShadowAdjust"
                        withInputParameters:@{
                            @"inputShadowAmount": @(lift),
                            @"inputHighlightAmount": @(0.90),
                        }];
                    float restrainedGain = 1.0f + (exposureGain - 1.0f) * 0.42f;
                    result = [result imageByApplyingFilter:@"CIExposureAdjust"
                        withInputParameters:@{kCIInputEVKey: @(log2f(restrainedGain))}];
                } else {
                    result = [result imageByApplyingFilter:@"CIExposureAdjust"
                        withInputParameters:@{kCIInputEVKey: @(log2f(exposureGain))}];
                }
                result = [result imageByApplyingFilter:@"CIColorControls"
                    withInputParameters:@{
                        kCIInputContrastKey: @(1.03),
                        kCIInputSaturationKey: @(1.02),
                    }];
            }
            [context render:result toBitmap:output rowBytes:outputStride bounds:bounds format:kCIFormatRGBA8 colorSpace:colorSpace];
            if (opaque && frame != 0) {
                size_t border = MAX(3, MIN(width, height) / (frame >= 4 ? 18 : 80));
                const unsigned char colors[3][3] = {{255, 77, 41}, {238, 240, 245}, {255, 176, 32}};
                const unsigned char *color = colors[MIN(frame, 3) - 1];
                for (size_t y = 0; y < height; y++) {
                    for (size_t x = 0; x < width; x++) {
                        if (x < border || x >= width - border || y < border || y >= height - border) {
                            unsigned char *pixel = output + y * outputStride + x * 4;
                            memcpy(pixel, color, 3);
                            if (frame == 4) { // A repeating geometric ribbon.
                                BOOL alternate = ((x / MAX(1, border)) + (y / MAX(1, border))) % 2;
                                const unsigned char pattern[2][3] = {{255, 77, 41}, {87, 131, 225}};
                                memcpy(pixel, pattern[alternate], 3);
                            } else if (frame == 5) { // Confetti, deterministic across frames.
                                size_t cell = MAX(4, border / 2);
                                size_t seed = (x / cell * 7 + y / cell * 13) % 4;
                                const unsigned char pattern[4][3] = {{247, 214, 81}, {137, 107, 218}, {244, 118, 129}, {85, 183, 156}};
                                if ((x % cell + y % cell) % cell > cell / 3) memcpy(pixel, pattern[seed], 3);
                                else { pixel[0] = 248; pixel[1] = 243; pixel[2] = 232; }
                            } else if (frame == 6) { // Alternating leafy arcs.
                                size_t cell = MAX(6, border);
                                double dx = (double)(x % cell) - cell / 2.0, dy = (double)(y % cell) - cell / 2.0;
                                BOOL leaf = dx * dx + dy * dy < cell * cell * 0.19;
                                pixel[0] = leaf ? 68 : 214; pixel[1] = leaf ? 120 : 231; pixel[2] = leaf ? 82 : 207;
                            }
                            pixel[3] = 255;
                        }
                    }
                }
            }
            return 0;
        } @catch (NSException *exception) {
            return 2;
        } @finally {
            if (colorSpace) CGColorSpaceRelease(colorSpace);
        }
    }
}
