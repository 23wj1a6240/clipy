// Live Apple Speech transcription for Local mode.
//
// `apple.m` transcribes a finished file: it re-opens the recording, decodes a
// ten-second PCM range and hands that to a fresh recognizer. On a fragmented
// MP4 that is still being written, re-opening is O(file) — AVFoundation has to
// walk every `moof` to build the sample table — and it happens twice per
// chunk, so catching up costs O(N^2) and the worker falls behind the take it
// is supposed to be keeping up with (measured: 50 s of pure AVFoundation
// overhead on a sealed 20-minute recording, and a 22 s post-Stop wait on a
// real 54-second one).
//
// Nothing here reads the file. The capture pump already holds the microphone
// PCM in memory, so this feeds those buffers straight into one long-lived
// `SFSpeechAudioBufferRecognitionRequest`. Recognition then runs while the
// user is still talking and only the last partial session remains at Stop.
//
// Sessions are rotated by the caller because a single `SFSpeechRecognitionTask`
// is not meant to run for an arbitrarily long time. A rotation can split a word
// across two sessions; so could the ten-second ranges this replaces, and the
// boundary is far rarer here.
//
// Threading: `append` is called from the feeder thread, `rotate`/`finish` from
// the local processing thread. One lock covers the whole session object.

#import <Foundation/Foundation.h>
#import <AVFoundation/AVFoundation.h>
#import <Speech/Speech.h>
#import "speech_permissions.h"

/// Apple delivers one final result per utterance, and the task keeps running
/// afterwards. Keeping only the newest one (what the block-based handler
/// invites) silently throws away everything before the last utterance, which
/// measured as 2 segments instead of 61 on a 54-second take. The delegate
/// distinguishes "another final result" from "the task is over", so every
/// utterance is collected and the wait ends exactly once.
@interface ClipySttSink : NSObject <SFSpeechRecognitionTaskDelegate>
@property (nonatomic, strong) NSMutableArray<SFTranscription *> *transcriptions;
@property (nonatomic, strong) dispatch_semaphore_t done;
@property (nonatomic, strong) NSError *failure;
@property (nonatomic) BOOL ended;
@end

@implementation ClipySttSink
- (instancetype)init {
    if ((self = [super init])) {
        _transcriptions = [NSMutableArray array];
        _done = dispatch_semaphore_create(0);
    }
    return self;
}
- (void)speechRecognitionTask:(SFSpeechRecognitionTask *)task didFinishRecognition:(SFSpeechRecognitionResult *)result {
    (void)task;
    if (result.bestTranscription) {
        @synchronized (self) { [self.transcriptions addObject:result.bestTranscription]; }
    }
}
- (void)speechRecognitionTask:(SFSpeechRecognitionTask *)task didFinishSuccessfully:(BOOL)successfully {
    @synchronized (self) {
        if (!successfully && !self.failure) self.failure = task.error;
        if (self.ended) return;
        self.ended = YES;
    }
    dispatch_semaphore_signal(self.done);
}
@end

static NSLock *gLock = nil;
static SFSpeechRecognizer *gRecognizer = nil;
static SFSpeechAudioBufferRecognitionRequest *gRequest = nil;
static SFSpeechRecognitionTask *gTask = nil;
static AVAudioFormat *gFormat = nil;
static ClipySttSink *gSink = nil;
// Seconds of audio handed to sessions that have already been harvested, so a
// segment's stream time is this plus its own in-session timestamp.
static double gBaseSeconds = 0;
static double gSessionSeconds = 0;
static BOOL gOpen = NO;

static char *Copy(NSString *value) { return strdup(value.UTF8String); }

static char *ErrorOut(NSString *message) {
    return Copy([NSString stringWithFormat:@"{\"error\":%@}",
        [[NSString alloc] initWithData:[NSJSONSerialization dataWithJSONObject:message options:NSJSONWritingFragmentsAllowed error:nil]
                              encoding:NSUTF8StringEncoding]]);
}

/// Starts one recognition task against the already-built format. Caller holds the lock.
static BOOL StartTask(NSError **error) {
    gRequest = [[SFSpeechAudioBufferRecognitionRequest alloc] init];
    gRequest.requiresOnDeviceRecognition = YES;
    // Partial results keep the recognizer reporting as it consumes audio, which
    // is also how the caller can tell streaming is actually live. Only the
    // delegate's final results are kept.
    gRequest.shouldReportPartialResults = YES;
    gSink = [[ClipySttSink alloc] init];
    gSessionSeconds = 0;
    gTask = [gRecognizer recognitionTaskWithRequest:gRequest delegate:gSink];
    if (!gTask) {
        gSink = nil;
        if (error) *error = [NSError errorWithDomain:@"ClipyLocal" code:2
            userInfo:@{NSLocalizedDescriptionKey: @"Could not start live transcription."}];
        return NO;
    }
    return YES;
}

/// Ends the current task and returns its segments as stream-relative JSON.
/// Caller holds the lock. `gBaseSeconds` advances by the audio this session saw.
static NSString *HarvestLocked(void) {
    if (!gTask) return @"{\"segments\":[]}";
    ClipySttSink *sink = gSink;
    [gRequest endAudio];
    // The recognizer has already consumed this audio, so the remaining work is
    // finalising the last utterance. The bound only exists so a wedged speech
    // daemon cannot hang Stop.
    BOOL completed = dispatch_semaphore_wait(sink.done, dispatch_time(DISPATCH_TIME_NOW, 30 * NSEC_PER_SEC)) == 0;
    if (!completed) [gTask cancel];
    NSArray<SFTranscription *> *transcriptions = nil;
    NSError *failure = nil;
    @synchronized (sink) { transcriptions = [sink.transcriptions copy]; failure = sink.failure; }
    NSMutableArray *segments = [NSMutableArray array];
    // Apple restarts each utterance's timestamps from the start of the request's
    // audio, so results are already on one timeline; a repeated start time is
    // the same word re-reported, not a new one.
    double lastStart = -1;
    for (SFTranscription *transcription in transcriptions) {
        for (SFTranscriptionSegment *segment in transcription.segments) {
            if (!segment.substring.length) continue;
            if (segment.timestamp <= lastStart) continue;
            lastStart = segment.timestamp;
            [segments addObject:@{
                @"startMs": @((uint64_t)((gBaseSeconds + segment.timestamp) * 1000)),
                @"endMs": @((uint64_t)((gBaseSeconds + segment.timestamp + segment.duration) * 1000)),
                @"text": segment.substring,
            }];
        }
    }
    NSMutableDictionary *out = [NSMutableDictionary dictionaryWithObject:segments forKey:@"segments"];
    // A warning here means "this session's window is not fully covered", and
    // the Rust side hands that window back to the file path. It has to be
    // reported whether or not some segments came through first: a task that
    // failed or timed out after three utterances still dropped everything
    // after them, and a clean-looking partial is worse than a gap because
    // nothing would ever go back for the rest.
    // kAFAssistantErrorDomain 1110 is ordinary "no speech in this audio", which
    // is a capture gap and not a failure, exactly as the file path treats it.
    if (failure && !([failure.domain isEqual:@"kAFAssistantErrorDomain"] && failure.code == 1110)) {
        out[@"warning"] = failure.localizedDescription ?: @"Live transcription ended early.";
    } else if (!completed) {
        out[@"warning"] = @"Live transcription timed out waiting for a result.";
    }
    gBaseSeconds += gSessionSeconds;
    gTask = nil;
    gRequest = nil;
    gSink = nil;
    NSData *data = [NSJSONSerialization dataWithJSONObject:out options:0 error:nil];
    return [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
}

/// Opens a live session for interleaved float PCM at the microphone's own rate.
/// Returns NULL on success, or a JSON `{"error":...}` string the caller frees.
char *clipy_stt_open(double sampleRate, unsigned int channels) {
    @autoreleasepool {
        NSString *configurationError = ClipySpeechConfigurationError();
        if (configurationError) return ErrorOut(configurationError);
        if (!gLock) gLock = [[NSLock alloc] init];
        [gLock lock];
        if (gOpen) { [gLock unlock]; return ErrorOut(@"A live transcription session is already open."); }
        if (sampleRate <= 0 || channels == 0 || channels > 8) {
            [gLock unlock]; return ErrorOut(@"Unsupported microphone format for live transcription.");
        }
        gRecognizer = [[SFSpeechRecognizer alloc] initWithLocale:[NSLocale currentLocale]];
        if (!gRecognizer.supportsOnDeviceRecognition) {
            gRecognizer = nil; [gLock unlock];
            return ErrorOut(@"Apple on-device transcription is unavailable for this Mac or system language.");
        }
        if ([SFSpeechRecognizer authorizationStatus] != SFSpeechRecognizerAuthorizationStatusAuthorized) {
            gRecognizer = nil; [gLock unlock];
            return ErrorOut(@"Speech Recognition permission is not granted.");
        }
        // User-initiated, not background: the file path pinned its delivery
        // queue to QOS_CLASS_BACKGROUND, which on Apple silicon confines the
        // work to the efficiency cores. Live audio has to keep up with wall
        // clock, so it must not be the first thing descheduled.
        NSOperationQueue *queue = [[NSOperationQueue alloc] init];
        queue.qualityOfService = NSQualityOfServiceUserInitiated;
        queue.maxConcurrentOperationCount = 1;
        gRecognizer.queue = queue;
        gFormat = [[AVAudioFormat alloc] initWithCommonFormat:AVAudioPCMFormatFloat32
                                                   sampleRate:sampleRate
                                                     channels:channels
                                                  interleaved:YES];
        if (!gFormat) {
            gRecognizer = nil; [gLock unlock];
            return ErrorOut(@"Could not describe the microphone format for live transcription.");
        }
        gBaseSeconds = 0;
        NSError *error = nil;
        if (!StartTask(&error)) {
            gRecognizer = nil; gFormat = nil; [gLock unlock];
            return ErrorOut(error.localizedDescription ?: @"Could not start live transcription.");
        }
        gOpen = YES;
        [gLock unlock];
        return NULL;
    }
}

/// Appends interleaved float frames. Returns 0 when accepted, 1 when the task
/// has already finished and the caller should rotate before appending more,
/// and -1 when no session is open. Never blocks on recognition:
/// `appendAudioPCMBuffer` hands the buffer to the speech daemon and returns.
int clipy_stt_append(const float *interleaved, unsigned int frames) {
    @autoreleasepool {
        if (!gLock) return -1;
        [gLock lock];
        if (!gOpen || !gRequest || !gFormat || !interleaved || frames == 0) { [gLock unlock]; return -1; }
        // A task that has already reported itself finished silently discards
        // anything appended after it, so say so rather than feeding a dead
        // session for the rest of the rotation window.
        ClipySttSink *sink = gSink;
        BOOL ended = NO;
        @synchronized (sink) { ended = sink.ended; }
        if (ended) { [gLock unlock]; return 1; }
        AVAudioPCMBuffer *buffer = [[AVAudioPCMBuffer alloc] initWithPCMFormat:gFormat frameCapacity:frames];
        if (!buffer) { [gLock unlock]; return -1; }
        buffer.frameLength = frames;
        memcpy(buffer.floatChannelData[0], interleaved,
               (size_t)frames * gFormat.channelCount * sizeof(float));
        [gRequest appendAudioPCMBuffer:buffer];
        gSessionSeconds += (double)frames / gFormat.sampleRate;
        [gLock unlock];
        return 0;
    }
}

/// Seconds of audio the current session has consumed, so the caller can decide
/// when to rotate. Returns a negative value when no session is open.
double clipy_stt_session_seconds(void) {
    if (!gLock) return -1;
    [gLock lock];
    double seconds = gOpen ? gSessionSeconds : -1;
    [gLock unlock];
    return seconds;
}

/// Harvests the current session and immediately opens the next one.
char *clipy_stt_rotate(void) {
    @autoreleasepool {
        if (!gLock) return ErrorOut(@"No live transcription session.");
        [gLock lock];
        if (!gOpen) { [gLock unlock]; return ErrorOut(@"No live transcription session."); }
        NSString *harvested = HarvestLocked();
        NSError *error = nil;
        if (!StartTask(&error)) {
            gOpen = NO; gRecognizer = nil; gFormat = nil; [gLock unlock];
            return ErrorOut(error.localizedDescription ?: @"Could not continue live transcription.");
        }
        [gLock unlock];
        return Copy(harvested);
    }
}

/// Ends the session and returns its remaining segments.
char *clipy_stt_finish(void) {
    @autoreleasepool {
        if (!gLock) return ErrorOut(@"No live transcription session.");
        [gLock lock];
        if (!gOpen) { [gLock unlock]; return ErrorOut(@"No live transcription session."); }
        NSString *harvested = HarvestLocked();
        gOpen = NO;
        gRecognizer = nil;
        gFormat = nil;
        [gLock unlock];
        return Copy(harvested);
    }
}

/// Abandons the session without waiting for a result (cancel/discard).
void clipy_stt_close(void) {
    @autoreleasepool {
        if (!gLock) return;
        [gLock lock];
        if (gTask) [gTask cancel];
        gTask = nil; gRequest = nil; gSink = nil;
        gRecognizer = nil; gFormat = nil; gOpen = NO; gBaseSeconds = 0; gSessionSeconds = 0;
        [gLock unlock];
    }
}

void clipy_stt_free(char *value) { free(value); }
