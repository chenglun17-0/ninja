// virtual-display.m —— ninja E2E 虚拟屏工具
//
// 用途：E2E/GUI 取证在虚拟屏上跑，不打扰开发者主屏（不弹窗、不抢焦点）。
//
// 子命令：
//   hold [width height hidpi]   创建虚拟屏并常驻；stdout 一行 JSON（displayID/frame/…），
//                               进程退出（含 SIGTERM）即拔屏。默认 1920x1080 hidpi=0（72dpi、像素 1:1）。
//   list                        CG 层清点在线显示器（id/bounds/main/vendor/model/serial）
//   screens                     AppKit 层清点 NSScreen（宿主窗口落位看到的视角）
//
// 原理：CoreGraphics 私有 SPI CGVirtualDisplay（ObjC 类，DeskPad 同路；
// 接口声明与用法源自 Stengo/DeskPad，MIT）。无 root、无 SIP 改动，Apple Silicon。
// E2E 约定（PLAN.md「E2E 虚拟屏幕」）：宿主读 NINJA_E2E_SCREEN=<displayID> 落窗；
// 截图按窗口 ID（screencapture -l）与屏幕无关；虚拟屏不可用回退主屏并标注。
//
// 编译（产物名无扩展名，已入 .gitignore）：
//   clang -fobjc-arc -framework Foundation -framework CoreGraphics -framework AppKit \
//         -Wl,-undefined,dynamic_lookup scripts/e2e/virtual-display.m -o scripts/e2e/virtual-display

#import <Cocoa/Cocoa.h>
#import <CoreGraphics/CoreGraphics.h>

// ---- CoreGraphics 私有 SPI（声明即可；类由 CoreGraphics 在运行时提供）----

@interface CGVirtualDisplayMode : NSObject
- (instancetype)initWithWidth:(NSUInteger)w height:(NSUInteger)h refreshRate:(CGFloat)r;
@end

@interface CGVirtualDisplaySettings : NSObject
@property(retain, nonatomic) NSArray<CGVirtualDisplayMode *> *modes;
@property(nonatomic) unsigned int hiDPI;
@end

@interface CGVirtualDisplay : NSObject
@property(readonly, nonatomic) CGDirectDisplayID displayID;
- (instancetype)initWithDescriptor:(id)descriptor;
- (BOOL)applySettings:(CGVirtualDisplaySettings *)s;
@end

@interface CGVirtualDisplayDescriptor : NSObject
@property(retain, nonatomic) dispatch_queue_t queue;
@property(retain, nonatomic) NSString *name;
@property(nonatomic) unsigned int maxPixelsHigh;
@property(nonatomic) unsigned int maxPixelsWide;
@property(nonatomic) CGSize sizeInMillimeters;
@property(nonatomic) unsigned int serialNum;
@property(nonatomic) unsigned int productID;
@property(nonatomic) unsigned int vendorID;
@end

// 保活：进程存活期间对象不释放，屏就在。
static CGVirtualDisplay *gDisplay = nil;

static void onSignal(int sig) {
    // async-signal-safe：直接退（进程退出 ⇒ 虚拟屏拔除）。
    _exit(sig == SIGINT ? 130 : 0);
}

static int cmdList(void) {
    @autoreleasepool {
        const uint32_t max = 16;
        CGDirectDisplayID ids[16];
        uint32_t count = 0;
        if (CGGetActiveDisplayList(max, ids, &count) != kCGErrorSuccess) {
            fprintf(stderr, "CGGetActiveDisplayList failed\n");
            return 1;
        }
        for (uint32_t i = 0; i < count; i++) {
            CGDirectDisplayID id = ids[i];
            CGRect b = CGDisplayBounds(id);
            printf("{\"id\":%u,\"x\":%.0f,\"y\":%.0f,\"w\":%.0f,\"h\":%.0f,"
                   "\"main\":%u,\"vendor\":%u,\"model\":%u,\"serial\":%u}\n",
                   id, b.origin.x, b.origin.y, b.size.width, b.size.height,
                   CGDisplayIsMain(id), CGDisplayVendorNumber(id),
                   CGDisplayModelNumber(id), CGDisplaySerialNumber(id));
        }
        return 0;
    }
}

static int cmdScreens(void) {
    @autoreleasepool {
        for (NSScreen *s in NSScreen.screens) {
            NSDictionary *d = s.deviceDescription;
            NSNumber *num = d[@"NSScreenNumber"];
            CGRect f = s.frame;
            printf("{\"id\":%u,\"x\":%.0f,\"y\":%.0f,\"w\":%.0f,\"h\":%.0f,"
                   "\"scale\":%.0f,\"main\":%d}\n",
                   num.unsignedIntValue, f.origin.x, f.origin.y,
                   f.size.width, f.size.height,
                   s.backingScaleFactor, s == NSScreen.mainScreen ? 1 : 0);
        }
        return 0;
    }
}

static int cmdHold(int argc, const char *argv[]) {
    @autoreleasepool {
        NSUInteger w = (argc >= 4) ? strtoul(argv[2], NULL, 10) : 1920;
        NSUInteger h = (argc >= 4) ? strtoul(argv[3], NULL, 10) : 1080;
        unsigned int hidpi = (argc >= 5) ? (unsigned int)strtoul(argv[4], NULL, 10) : 0;
        if (w == 0 || h == 0) { fprintf(stderr, "usage: hold [width height hidpi]\n"); return 2; }

        CGVirtualDisplayDescriptor *desc = [[CGVirtualDisplayDescriptor alloc] init];
        desc.queue = dispatch_get_main_queue();
        desc.name = @"ninja-e2e";
        desc.maxPixelsWide = 3840;
        desc.maxPixelsHigh = 2160;
        desc.sizeInMillimeters = CGSizeMake(600, 340);
        desc.vendorID = 0xE2E2;
        desc.productID = 0xE2E1;
        desc.serialNum = 0xE2E1;

        CGVirtualDisplay *vd = [[CGVirtualDisplay alloc] initWithDescriptor:desc];
        if (vd == nil) { fprintf(stderr, "CGVirtualDisplay create failed\n"); return 1; }

        CGVirtualDisplaySettings *settings = [[CGVirtualDisplaySettings alloc] init];
        settings.hiDPI = hidpi;
        settings.modes = @[ [[CGVirtualDisplayMode alloc] initWithWidth:w height:h refreshRate:60.0] ];
        if (![vd applySettings:settings]) {
            fprintf(stderr, "applySettings failed (w=%lu h=%lu hidpi=%u)\n",
                    (unsigned long)w, (unsigned long)h, hidpi);
            return 1;
        }
        gDisplay = vd; // 保活

        // 等 WindowServer 把屏挂稳（bounds/NSScreen 可见），最多 5s。
        __block CGRect frame = CGRectNull;
        for (int i = 0; i < 50 && CGRectIsNull(frame); i++) {
            usleep(100 * 1000);
            for (NSScreen *s in NSScreen.screens) {
                if ([s.deviceDescription[@"NSScreenNumber"] unsignedIntValue] == vd.displayID) {
                    frame = s.frame;
                }
            }
        }
        if (CGRectIsNull(frame)) {
            printf("{\"displayID\":%u,\"width\":%lu,\"height\":%lu,\"hidpi\":%u,\"frame\":null}\n",
                   vd.displayID, (unsigned long)w, (unsigned long)h, hidpi);
        } else {
            printf("{\"displayID\":%u,\"width\":%lu,\"height\":%lu,\"hidpi\":%u,"
                   "\"frame\":[%.0f,%.0f,%.0f,%.0f]}\n",
                   vd.displayID, (unsigned long)w, (unsigned long)h, hidpi,
                   frame.origin.x, frame.origin.y, frame.size.width, frame.size.height);
        }
        fflush(stdout);

        signal(SIGINT, onSignal);
        signal(SIGTERM, onSignal);
        CFRunLoopRun(); // 常驻；被杀即拔屏
        return 0;
    }
}

int main(int argc, const char *argv[]) {
    if (argc < 2) {
        fprintf(stderr, "usage: virtual-display hold [w h hidpi] | list | screens\n");
        return 2;
    }
    if (strcmp(argv[1], "list") == 0) return cmdList();
    if (strcmp(argv[1], "screens") == 0) return cmdScreens();
    if (strcmp(argv[1], "hold") == 0) return cmdHold(argc, argv);
    fprintf(stderr, "unknown subcommand: %s\n", argv[1]);
    return 2;
}
