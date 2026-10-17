#include "app_macos.h"
#import <AppKit/AppKit.h>
#include <SDL3/SDL.h>
#include <stdlib.h>
#include <string.h>

static NSMutableArray<NSString *> *pendingFiles;
static NSOpenPanel *openPanel;

bool up_mac_app_is_bundle(void)
{
    return [[[NSBundle mainBundle] objectForInfoDictionaryKey:@"CFBundlePackageType"] isEqualToString:@"APPL"];
}

static bool receive_file(void *unused, SDL_Event *event)
{
    (void) unused;
    if (event->type == SDL_EVENT_DROP_FILE && event->drop.data) {
        NSString *path = @(event->drop.data);
        @synchronized(pendingFiles) { [pendingFiles addObject:path]; }
        // Finder can deliver its open-file event while the initial panel is up.
        dispatch_async(dispatch_get_main_queue(), ^{ [openPanel cancel:nil]; });
    }
    return true;
}

@interface UpFileMenu : NSObject
- (void)openDocument:(id)sender;
@end
@implementation UpFileMenu
- (void)openDocument:(id)sender
{
    (void) sender;
    if (openPanel) { [openPanel makeKeyAndOrderFront:nil]; return; }
    openPanel = [NSOpenPanel openPanel];
    openPanel.canChooseDirectories = NO;
    openPanel.allowsMultipleSelection = NO;
    openPanel.title = @"Open Video";
    [openPanel beginWithCompletionHandler:^(NSModalResponse result) {
        if (result == NSModalResponseOK) {
            @synchronized(pendingFiles) { [pendingFiles addObject:openPanel.URL.path]; }
        }
        openPanel = nil;
    }];
}
@end
static UpFileMenu *fileMenu;

void up_mac_app_init(void)
{
    if (!up_mac_app_is_bundle() || pendingFiles) return;
    NSString *fonts = [[NSBundle mainBundle] pathForResource:@"fonts" ofType:@"conf"];
    if (fonts) setenv("FONTCONFIG_FILE", fonts.fileSystemRepresentation, 0);
    pendingFiles = [NSMutableArray new];
    SDL_AddEventWatch(receive_file, NULL);
    fileMenu = [UpFileMenu new];
    NSMenu *menu = [[NSMenu alloc] initWithTitle:@"File"];
    NSMenuItem *item = [menu addItemWithTitle:@"Open…" action:@selector(openDocument:) keyEquivalent:@"o"];
    item.target = fileMenu;
    NSMenuItem *root = [[NSMenuItem alloc] initWithTitle:@"File" action:nil keyEquivalent:@""];
    root.submenu = menu;
    [NSApp.mainMenu insertItem:root atIndex:1];
}

char *up_mac_app_take_file(void)
{
    @autoreleasepool {
        @synchronized(pendingFiles) {
            if (!pendingFiles.count) return NULL;
            char *path = strdup(pendingFiles.firstObject.fileSystemRepresentation);
            [pendingFiles removeObjectAtIndex:0];
            return path;
        }
    }
}

char *up_mac_app_choose_file(void)
{
    @autoreleasepool {
        SDL_PumpEvents();
        char *path = up_mac_app_take_file();
        if (path) return path;
        [fileMenu openDocument:nil];
        while (openPanel) {
            SDL_Event event;
            while (SDL_PollEvent(&event)) {
                if (event.type == SDL_EVENT_QUIT) [openPanel cancel:nil];
            }
            SDL_Delay(10);
        }
        return up_mac_app_take_file();
    }
}

void up_mac_app_free_path(char *path) { free(path); }

void up_mac_app_show_error(const char *message)
{
    @autoreleasepool {
        NSAlert *alert = [NSAlert new];
        alert.messageText = @"Unable to play video";
        alert.informativeText = @(message);
        [alert runModal];
    }
}

void up_mac_app_quit(void)
{
    if (!pendingFiles) return;
    SDL_RemoveEventWatch(receive_file, NULL);
    [openPanel cancel:nil];
    openPanel = nil;
    pendingFiles = nil;
    fileMenu = nil;
}
