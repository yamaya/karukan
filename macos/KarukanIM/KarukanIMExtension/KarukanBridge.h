//
//  KarukanBridge.h
//  KarukanIMExtension
//
//  Swift Bridging Header — karukan-macos C FFI を Swift へ公開する。
//  Build Settings > Swift Compiler - General > Objective-C Bridging Header に
//  このファイルのパスを設定すること:
//    KarukanIMExtension/KarukanBridge.h
//

#ifndef KarukanBridge_h
#define KarukanBridge_h

// karukan-macos の C FFI ヘッダー
// HEADER_SEARCH_PATHS に $(SRCROOT)/../../karukan-macos/include を設定している場合
#include "karukan_macos.h"

#endif /* KarukanBridge_h */
