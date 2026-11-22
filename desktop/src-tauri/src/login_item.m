// Small native boundary for macOS login items. Policy and persistence live in Rust.
#import <Foundation/Foundation.h>
#import <ServiceManagement/ServiceManagement.h>
#import <CoreServices/CoreServices.h>

// Shared with LoginItemStatus in login_item.rs: disabled, enabled, approval,
// unavailable. Negative results are native failures, never a disabled setting.
static NSURL *installedBundleURL(void) {
    NSURL *url = NSBundle.mainBundle.bundleURL.URLByResolvingSymlinksInPath;
    NSString *path = url.path;
    NSString *userApps = [NSHomeDirectory() stringByAppendingPathComponent:@"Applications/"];
    if (![url.pathExtension isEqualToString:@"app"] ||
        !([path hasPrefix:@"/Applications/"] || [path hasPrefix:userApps])) {
        return nil;
    }
    return url;
}

// Monterey has no SMAppService. Keep its login item in the user's shared
// file list, without AppleScript or an Automation permission prompt.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
static int legacyLoginItem(NSURL *url, int desired) {
    LSSharedFileListRef list = LSSharedFileListCreate(NULL, kLSSharedFileListSessionLoginItems, NULL);
    if (!list) return -1;
    CFArrayRef items = LSSharedFileListCopySnapshot(list, NULL);
    if (!items) {
        CFRelease(list);
        return -1;
    }
    BOOL found = NO;
    BOOL failed = NO;
    for (CFIndex i = 0; i < CFArrayGetCount(items); i++) {
        LSSharedFileListItemRef item = (LSSharedFileListItemRef)CFArrayGetValueAtIndex(items, i);
        CFURLRef itemURL = NULL;
        OSStatus result = LSSharedFileListItemResolve(item,
            kLSSharedFileListNoUserInteraction | kLSSharedFileListDoNotMountVolumes, &itemURL, NULL);
        if (result == noErr && itemURL) {
            NSURL *resolved = [(__bridge NSURL *)itemURL URLByResolvingSymlinksInPath];
            if ([resolved isEqual:url]) {
                found = YES;
                if (desired == 0 && LSSharedFileListItemRemove(list, item) != noErr) failed = YES;
            }
        }
        if (itemURL) CFRelease(itemURL);
    }
    if (desired == 1 && !found) {
        LSSharedFileListItemRef item = LSSharedFileListInsertItemURL(list,
            kLSSharedFileListItemLast, NULL, NULL, (__bridge CFURLRef)url, NULL, NULL);
        if (item) CFRelease(item);
        else failed = YES;
    }
    CFRelease(items);
    CFRelease(list);
    return failed ? -1 : (desired < 0 ? (found ? 1 : 0) : desired);
}
#pragma clang diagnostic pop

int clipy_login_item_status(void) {
    @autoreleasepool {
        NSURL *url = installedBundleURL();
        if (!url) return 3;
        if (@available(macOS 13.0, *)) {
            switch (SMAppService.mainAppService.status) {
                case SMAppServiceStatusNotRegistered:
                // A newly installed bundle can be unknown until its first registration.
                case SMAppServiceStatusNotFound: return 0;
                case SMAppServiceStatusEnabled: return 1;
                case SMAppServiceStatusRequiresApproval: return 2;
            }
            return -1;
        }
        return legacyLoginItem(url, -1);
    }
}

int clipy_login_item_set_enabled(int enabled) {
    @autoreleasepool {
        NSURL *url = installedBundleURL();
        if (!url) return 3;
        if (@available(macOS 13.0, *)) {
            SMAppService *service = SMAppService.mainAppService;
            if (enabled && service.status == SMAppServiceStatusEnabled) return 1;
            if (!enabled && service.status == SMAppServiceStatusNotRegistered) return 0;
            // Approval is controlled by System Settings, not repeated registration.
            if (enabled && service.status == SMAppServiceStatusRequiresApproval) return 2;
            NSError *error = nil;
            BOOL success = enabled ? [service registerAndReturnError:&error]
                                   : [service unregisterAndReturnError:&error];
            if (!success) return -1;
            return clipy_login_item_status();
        }
        return legacyLoginItem(url, enabled ? 1 : 0);
    }
}
