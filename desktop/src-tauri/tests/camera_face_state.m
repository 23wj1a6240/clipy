#import "../src/camera_effects.m"
#include <assert.h>
#include <stdio.h>

int main(void) {
    @autoreleasepool {
        void *owned = clipy_camera_face_state_create();
        ClipyFaceState *state = (__bridge ClipyFaceState *)owned;
        CGRect bounds = CGRectMake(0, 0, 640, 480);
        CGRect face = CGRectMake(400, 240, 120, 160);
        CIImage *source = [[CIImage imageWithColor:[CIColor colorWithRed:0 green:0 blue:0 alpha:1]]
            imageByCroppingToRect:bounds];
        state.bounds = bounds;
        state.face = face;
        state.frame = 1;

        assert(CGRectEqualToRect(ClipyFaceRect(state, source, bounds, NO), face));
        dispatch_semaphore_t finished = dispatch_semaphore_create(0);
        dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
            @autoreleasepool {
                assert(!NSThread.isMainThread);
                assert(CGRectEqualToRect(ClipyFaceRect(state, source, bounds, NO), face));
                assert(state.frame == 3);
            }
            dispatch_semaphore_signal(finished);
        });
        assert(dispatch_semaphore_wait(finished,
            dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC)) == 0);

        void *otherOwned = clipy_camera_face_state_create();
        ClipyFaceState *other = (__bridge ClipyFaceState *)otherOwned;
        assert(CGRectIsNull(other.face));
        assert(other.frame == 0 && other.misses == 0);
        assert(other.request != state.request);
        assert(CGRectIsNull(ClipyFaceRect(other, source, bounds, NO)));
        assert(CGRectEqualToRect(state.face, face));
        assert(state.frame == 3 && state.misses == 0);

        CGRect smaller = CGRectMake(0, 0, 128, 96);
        CIImage *smallSource = [source imageByCroppingToRect:smaller];
        state.misses = 1;
        assert(CGRectIsNull(ClipyFaceRect(state, smallSource, smaller, NO)));
        assert(CGRectEqualToRect(state.bounds, smaller));
        assert(CGRectIsNull(state.face));
        assert(state.frame == 1 && state.misses == 1);

        state.face = CGRectMake(100, 80, 100, 100);
        assert(CGRectEqualToRect(ClipyFaceRect(state, smallSource, smaller, NO),
            CGRectMake(100, 80, 28, 16)));

        unsigned char input[128 * 96 * 4] = {0};
        unsigned char output[sizeof(input)] = {0};
        for (size_t i = 3; i < sizeof(input); i += 4) input[i] = 255;
        float luma = -1;
        int status = clipy_camera_effect(owned, input, 128, 96, 128 * 4,
            output, 128 * 4, 0, 0, 0, 0, true, NULL, 0, 0, 1, 1, &luma);
        assert(status == 0);
        assert(luma >= 0 && luma <= 1);

        __weak ClipyFaceState *released = other;
        other = nil;
        clipy_camera_face_state_destroy(otherOwned);
        assert(released == nil);
        state = nil;
        clipy_camera_face_state_destroy(owned);
        puts("PASS: thread handoff, processor isolation, resize reset, bounds clamp, rendering, native release");
    }
    return 0;
}
