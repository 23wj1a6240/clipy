#import <Foundation/Foundation.h>

// TCC can attribute an unbundled executable's Speech request to its terminal
// or IDE, even when Tauri embeds Info.plist in the Mach-O. That can abort the
// process, which neither an Objective-C exception nor a Rust Result can catch.
static inline NSString *ClipySpeechConfigurationError(void) {
    NSBundle *bundle = NSBundle.mainBundle;
    if (![bundle.bundleURL.pathExtension isEqualToString:@"app"]) {
        return @"Open Clipy from its application bundle to use Local mode. For development, run pnpm run tauri:dev:app from desktop/.";
    }
    id description = [bundle objectForInfoDictionaryKey:@"NSSpeechRecognitionUsageDescription"];
    if (![description isKindOfClass:NSString.class] ||
        [(NSString *)description stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceAndNewlineCharacterSet].length == 0) {
        return @"This Clipy build is missing its Speech Recognition privacy description. Reinstall Clipy or rebuild the application bundle before using Local mode.";
    }
    return nil;
}
