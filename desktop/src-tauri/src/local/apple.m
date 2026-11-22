#import <Foundation/Foundation.h>
#import <AVFoundation/AVFoundation.h>
#import <Speech/Speech.h>
#import <ImageIO/ImageIO.h>
#import <pthread.h>
#import "speech_permissions.h"

static NSError *LocalError(NSString *message) {
    return [NSError errorWithDomain:@"ClipyLocal" code:1 userInfo:@{NSLocalizedDescriptionKey: message}];
}

static AVAssetImageGenerator *ExactFrameGenerator(AVAsset *asset) {
    AVAssetImageGenerator *generator = [[AVAssetImageGenerator alloc] initWithAsset:asset];
    generator.appliesPreferredTrackTransform = YES;
    generator.maximumSize = CGSizeMake(1920, 1920);
    generator.requestedTimeToleranceBefore = kCMTimeZero;
    generator.requestedTimeToleranceAfter = kCMTimeZero;
    return generator;
}

static NSDictionary *FrameEntry(CGImageRef image, CMTime actual, AVAssetImageGeneratorResult outcome, NSError *failure, NSString *output) {
    if (outcome == AVAssetImageGeneratorSucceeded && image) {
        CGImageDestinationRef dest = CGImageDestinationCreateWithURL((__bridge CFURLRef)[NSURL fileURLWithPath:output], CFSTR("public.jpeg"), 1, NULL);
        BOOL ok = NO;
        if (dest) {
            CGImageDestinationAddImage(dest, image, (__bridge CFDictionaryRef)@{(__bridge id)kCGImageDestinationLossyCompressionQuality: @0.85});
            ok = CGImageDestinationFinalize(dest);
            CFRelease(dest);
        }
        return ok ? @{@"seconds": @(CMTimeGetSeconds(actual))} : @{@"error": @"Could not save local frame."};
    }
    if (outcome == AVAssetImageGeneratorCancelled) return @{@"error": @"Local frame extraction cancelled."};
    // The only failure the caller may skip: AVFoundation tried and could not
    // produce this frame. Cancel, timeout and save errors stay fatal.
    return @{@"error": failure.localizedDescription ?: @"Could not decode local frame.", @"undecodable": @YES};
}

// Nil when `done` fired; otherwise why the wait stopped.
static NSString *WaitForFrames(dispatch_semaphore_t done, NSString *cancel, NSUInteger seconds) {
    for (NSUInteger second = 0; second < seconds; second++) {
        if (cancel && [[NSFileManager defaultManager] fileExistsAtPath:cancel]) return @"Local frame extraction cancelled.";
        if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC)) == 0) return nil;
    }
    return @"Local frame extraction timed out. Saved recording files remain on this Mac.";
}

static NSDictionary *RetryFrame(AVAsset *asset, NSDictionary *request, NSString *cancel, NSString **stopped) {
    AVAssetImageGenerator *generator = ExactFrameGenerator(asset);
    CMTime time = CMTimeMakeWithSeconds([request[@"seconds"] doubleValue], 600);
    NSMutableDictionary *slot = [NSMutableDictionary dictionary];
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    [generator generateCGImagesAsynchronouslyForTimes:@[[NSValue valueWithCMTime:time]] completionHandler:^(CMTime requested, CGImageRef image, CMTime actual, AVAssetImageGeneratorResult outcome, NSError *failure) {
        (void)requested;
        // A handler that fires after the caller gave up must not write a JPEG
        // or touch the result it already returned.
        @synchronized (slot) { if (slot[@"abandoned"]) return; }
        NSDictionary *entry = FrameEntry(image, actual, outcome, failure, request[@"output"]);
        @synchronized (slot) { slot[@"entry"] = entry; }
        dispatch_semaphore_signal(done);
    }];
    *stopped = WaitForFrames(done, cancel, 30);
    @synchronized (slot) {
        if (*stopped) {
            slot[@"abandoned"] = @YES;
            [generator cancelAllCGImageGeneration];
            return @{@"error": *stopped};
        }
        return slot[@"entry"] ?: @{@"error": @"Frame was not generated."};
    }
}

static NSDictionary *Run(NSDictionary *input, NSError **error) {
    NSString *op = input[@"op"];
    if ([op isEqual:@"check"] || [op isEqual:@"transcribe"]) {
        NSString *configurationError = ClipySpeechConfigurationError();
        if (configurationError) { *error = LocalError(configurationError); return nil; }
    }
    // Background while the capture pump owns the machine; user-initiated once
    // the recording has stopped and the popup is showing a spinner for this
    // exact work. Background QoS is confined to the efficiency cores and yields
    // to everything, which turned 25 frame seeks into 84 seconds under load.
    if ([op isEqual:@"background"]) {
        pthread_set_qos_class_self_np(QOS_CLASS_BACKGROUND, 0);
        return @{};
    }
    if ([op isEqual:@"foreground"]) {
        pthread_set_qos_class_self_np(QOS_CLASS_USER_INITIATED, 0);
        return @{};
    }
    if ([op isEqual:@"trash"]) {
        if (![[NSFileManager defaultManager] trashItemAtURL:[NSURL fileURLWithPath:input[@"directory"]] resultingItemURL:nil error:error]) return nil;
        return @{};
    }
    if ([op isEqual:@"check"]) {
        SFSpeechRecognizer *recognizer = [[SFSpeechRecognizer alloc] initWithLocale:[NSLocale currentLocale]];
        if (!recognizer.supportsOnDeviceRecognition) {
            *error = LocalError(@"Apple on-device transcription is unavailable for this Mac or system language. No audio will be sent to Apple. Enable Dictation for your language in System Settings and try again."); return nil;
        }
        if ([input[@"authorize"] boolValue]) {
            dispatch_semaphore_t done = dispatch_semaphore_create(0);
            [SFSpeechRecognizer requestAuthorization:^(SFSpeechRecognizerAuthorizationStatus status) { (void)status; dispatch_semaphore_signal(done); }];
            if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 120 * NSEC_PER_SEC))) {
                *error = LocalError(@"Speech permission request timed out."); return nil;
            }
        }
        if ([input[@"authorize"] boolValue] && [SFSpeechRecognizer authorizationStatus] != SFSpeechRecognizerAuthorizationStatusAuthorized) {
            *error = LocalError(@"Allow Speech Recognition for Clipy in System Settings to use Local mode. Audio stays on this Mac."); return nil;
        }
        return @{@"onDevice": @YES, @"locale": recognizer.locale.localeIdentifier};
    }
    AVURLAsset *asset = [AVURLAsset URLAssetWithURL:[NSURL fileURLWithPath:input[@"video"]] options:@{AVURLAssetPreferPreciseDurationAndTimingKey: @YES}];
    if ([op isEqual:@"duration"]) {
        double seconds = CMTimeGetSeconds(asset.duration);
        return @{@"seconds": @(isfinite(seconds) ? MAX(0, seconds) : 0)};
    }
    if ([op isEqual:@"frames"]) {
        // One generator for the whole plan: AVFoundation decodes forward from
        // each keyframe once instead of re-parsing the asset per screenshot.
        NSArray *requests = input[@"frames"];
        if (![requests isKindOfClass:NSArray.class] || requests.count == 0) return @{@"frames": @[]};
        NSString *cancel = input[@"cancel"];
        AVAssetImageGenerator *generator = ExactFrameGenerator(asset);
        NSMutableArray *times = [NSMutableArray arrayWithCapacity:requests.count];
        for (NSDictionary *request in requests) {
            [times addObject:[NSValue valueWithCMTime:CMTimeMakeWithSeconds([request[@"seconds"] doubleValue], 600)]];
        }
        NSDictionary *placeholder = @{@"error": @"Frame was not generated."};
        NSMutableArray *results = [NSMutableArray arrayWithCapacity:requests.count];
        for (NSUInteger i = 0; i < requests.count; i++) [results addObject:placeholder];
        NSMutableIndexSet *undecoded = [NSMutableIndexSet indexSet];
        dispatch_semaphore_t done = dispatch_semaphore_create(0);
        __block NSUInteger next = 0;
        // Results arrive in request order, one per time, on AVFoundation's queue.
        [generator generateCGImagesAsynchronouslyForTimes:times completionHandler:^(CMTime requested, CGImageRef image, CMTime actual, AVAssetImageGeneratorResult outcome, NSError *failure) {
            (void)requested;
            NSUInteger index;
            @synchronized (results) { index = next++; }
            if (index >= requests.count) return;
            NSDictionary *entry = FrameEntry(image, actual, outcome, failure, requests[index][@"output"]);
            @synchronized (results) {
                results[index] = entry;
                if (outcome == AVAssetImageGeneratorFailed) [undecoded addIndex:index];
            }
            if (index + 1 == requests.count) dispatch_semaphore_signal(done);
        }];
        // Cancel and timeout are reported per slot rather than as an op error,
        // so the frames that did land are still indexed by the caller.
        NSString *stopped = WaitForFrames(done, cancel, 300);
        if (stopped) [generator cancelAllCGImageGeneration];
        NSIndexSet *retry;
        @synchronized (results) {
            if (stopped) {
                for (NSUInteger i = 0; i < results.count; i++) {
                    if ([results[i][@"error"] isEqual:placeholder]) results[i] = @{@"error": stopped};
                }
                return @{@"frames": [results copy]};
            }
            retry = [undecoded copy];
        }
        // A batched request that lands on the frame the previous request just
        // returned (two moments sharing a screenshot time, or two times inside
        // one frame) fails with -11821 "Cannot Decode" on a healthy file. A
        // fresh generator decodes it, so retry those slots on their own.
        __block NSString *retryStopped = nil;
        [retry enumerateIndexesUsingBlock:^(NSUInteger index, BOOL *stop) {
            (void)stop;
            // Once a retry is cancelled or times out, this slot and every one
            // after it report that, so the caller fails instead of skipping.
            if (retryStopped) {
                @synchronized (results) { results[index] = @{@"error": retryStopped}; }
                return;
            }
            for (int attempt = 0; attempt < 2; attempt++) {
                NSString *stopped = nil;
                NSDictionary *entry = RetryFrame(asset, requests[index], cancel, &stopped);
                if (stopped) retryStopped = stopped;
                @synchronized (results) { results[index] = entry; }
                if (stopped || !entry[@"error"]) return;
            }
        }];
        @synchronized (results) { return @{@"frames": [results copy]}; }
    }
    if (![op isEqual:@"transcribe"]) { *error = LocalError(@"Unknown local operation."); return nil; }
    NSArray *tracks = [asset tracksWithMediaType:AVMediaTypeAudio];
    if (tracks.count == 0) return @{@"segments": @[], @"gap": @"No audio track captured."};
    AVAssetReader *reader = [[AVAssetReader alloc] initWithAsset:asset error:error];
    if (!reader) return nil;
    double start = [input[@"start"] doubleValue];
    reader.timeRange = CMTimeRangeMake(CMTimeMakeWithSeconds(start, 48000), CMTimeMakeWithSeconds([input[@"duration"] doubleValue], 48000));
    NSDictionary *format = @{AVFormatIDKey: @(kAudioFormatLinearPCM), AVSampleRateKey: @16000,
        AVNumberOfChannelsKey: @1, AVLinearPCMBitDepthKey: @16, AVLinearPCMIsFloatKey: @NO,
        AVLinearPCMIsBigEndianKey: @NO, AVLinearPCMIsNonInterleaved: @NO};
    AVAssetReaderAudioMixOutput *output = [[AVAssetReaderAudioMixOutput alloc] initWithAudioTracks:tracks audioSettings:format];
    [reader addOutput:output];
    if (![reader startReading]) { *error = reader.error; return nil; }
    NSFileHandle *file = nil;
    NSString *audioPath = input[@"audio"];
    [[NSFileManager defaultManager] createFileAtPath:audioPath contents:[NSMutableData dataWithLength:44] attributes:@{NSFilePosixPermissions: @0600}];
    file = [NSFileHandle fileHandleForWritingAtPath:audioPath];
    [file seekToEndOfFile];
    uint32_t bytes = 0;
    double audioStart = start;
    CMSampleBufferRef sample;
    while ((sample = [output copyNextSampleBuffer])) {
        if (bytes == 0) {
            double pts = CMTimeGetSeconds(CMSampleBufferGetPresentationTimeStamp(sample));
            if (isfinite(pts)) audioStart = MAX(start, pts);
        }
        CMBlockBufferRef block = CMSampleBufferGetDataBuffer(sample);
        size_t length = block ? CMBlockBufferGetDataLength(block) : 0;
        NSMutableData *data = [NSMutableData dataWithLength:length];
        if (block && CMBlockBufferCopyDataBytes(block, 0, length, data.mutableBytes) == kCMBlockBufferNoErr) {
            [file writeData:data]; bytes += (uint32_t)length;
        }
        CFRelease(sample);
    }
    if (reader.status == AVAssetReaderStatusFailed) { [file closeFile]; *error = reader.error; return nil; }
    // Mono 16-bit 16 kHz little-endian PCM WAV, retained as local evidence.
    uint8_t header[44] = {'R','I','F','F',0,0,0,0,'W','A','V','E','f','m','t',' ',16,0,0,0,1,0,1,0,0x80,0x3e,0,0,0,0x7d,0,0,2,0,16,0,'d','a','t','a'};
    uint32_t riff = bytes + 36; memcpy(header + 4, &riff, 4); memcpy(header + 40, &bytes, 4);
    [file seekToFileOffset:0]; [file writeData:[NSData dataWithBytes:header length:44]]; [file closeFile];
    if (!bytes) return @{@"segments": @[], @"gap": @"No audio samples in this range."};
    SFSpeechRecognizer *recognizer = [[SFSpeechRecognizer alloc] initWithLocale:[NSLocale currentLocale]];
    if (!recognizer.supportsOnDeviceRecognition) { *error = LocalError(@"On-device speech became unavailable; audio remains local."); return nil; }
    NSOperationQueue *queue = [[NSOperationQueue alloc] init];
    queue.qualityOfService = NSQualityOfServiceBackground; queue.maxConcurrentOperationCount = 1;
    recognizer.queue = queue;
    SFSpeechURLRecognitionRequest *request = [[SFSpeechURLRecognitionRequest alloc] initWithURL:[NSURL fileURLWithPath:audioPath]];
    request.requiresOnDeviceRecognition = YES;
    request.shouldReportPartialResults = NO;
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    __block NSDictionary *result = nil;
    __block NSError *speechError = nil;
    SFSpeechRecognitionTask *task = [recognizer recognitionTaskWithRequest:request resultHandler:^(SFSpeechRecognitionResult *value, NSError *failure) {
        if (value.isFinal) {
            NSMutableArray *segments = [NSMutableArray array];
            for (SFTranscriptionSegment *segment in value.bestTranscription.segments) {
                double end = start + [input[@"duration"] doubleValue];
                if (audioStart + segment.timestamp >= end) continue;
                [segments addObject:@{@"startMs": @((uint64_t)((audioStart + segment.timestamp) * 1000)),
                    @"endMs": @((uint64_t)(MIN(end, audioStart + segment.timestamp + segment.duration) * 1000)), @"text": segment.substring}];
            }
            result = @{@"segments": segments}; dispatch_semaphore_signal(done);
        } else if (failure) { speechError = failure; dispatch_semaphore_signal(done); }
    }];
    BOOL completed = NO;
    for (NSUInteger second = 0; second < 180; second++) {
        if (input[@"cancel"] && [[NSFileManager defaultManager] fileExistsAtPath:input[@"cancel"]]) {
            [task cancel]; *error = LocalError(@"Local transcription cancelled."); return nil;
        }
        if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC)) == 0) { completed = YES; break; }
    }
    if (!completed) {
        [task cancel]; *error = LocalError(@"Apple transcription timed out. Audio is saved locally for retry."); return nil;
    }
    if (!result && [speechError.domain isEqual:@"kAFAssistantErrorDomain"] && speechError.code == 1110) {
        return @{@"segments": @[], @"gap": @"Apple detected no speech in this audio range; the recorded audio is unchanged."};
    }
    if (!result) { *error = speechError ?: LocalError(@"Apple returned no transcript."); return nil; }
    return result;
}

char *clipy_local_native(const char *json) {
    @autoreleasepool {
        NSError *error = nil;
        NSDictionary *input = [NSJSONSerialization JSONObjectWithData:[[NSString stringWithUTF8String:json] dataUsingEncoding:NSUTF8StringEncoding] options:0 error:&error];
        NSDictionary *result = nil;
        @try {
            result = input ? Run(input, &error) : nil;
        } @catch (NSException *exception) {
            (void)exception;
            error = LocalError(@"Apple local processing failed. Saved recording files remain on this Mac.");
        }
        NSData *data = [NSJSONSerialization dataWithJSONObject:result ?: @{@"error": error.localizedDescription ?: @"Local operation failed.", @"code": @(error.code), @"domain": error.domain ?: @""} options:0 error:nil];
        return strdup([[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding].UTF8String);
    }
}
void clipy_local_free(char *value) { free(value); }
