// Compiles runvil_plugin.h as C++. The address taken below names
// `runvil_plugin` as C++ sees it: without the header's `extern "C"` block
// it would be a mangled name that `layout.c` never defines, and the link
// would fail.

#include <cstddef>

#include "runvil_plugin.h"

typedef RvStatus (*RvEntry)(RvCall *call);

extern "C" std::size_t rv_cpp_size_of_call(void) {
    return sizeof(RvCall);
}

extern "C" std::size_t rv_cpp_size_of_pack_render_args(void) {
    return sizeof(RvPackRenderArgs);
}

extern "C" std::size_t rv_cpp_offset_of_pack_render_add_tool(void) {
    return offsetof(RvPackRenderArgs, add_tool);
}

extern "C" RvEntry rv_cpp_entry(void) {
    return &runvil_plugin;
}
