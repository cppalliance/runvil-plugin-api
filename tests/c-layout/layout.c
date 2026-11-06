/* The layout of every struct in runvil_plugin.h and the value of every
   constant, as a C99 compiler sees them, for the Rust tests to compare
   with runvil-plugin-sys. */

#include <stddef.h>

#include "runvil_plugin.h"

typedef struct RvLayoutEntry {
    const char *what;
    size_t value;
} RvLayoutEntry;

#define SIZE(T) {"sizeof(" #T ")", sizeof(T)}
#define OFFSET(T, field) {"offsetof(" #T ", " #field ")", offsetof(T, field)}
#define VALUE(constant) {#constant, (size_t)(constant)}

static const RvLayoutEntry ENTRIES[] = {
    SIZE(RvStatus),
    SIZE(RvVerb),
    SIZE(RvStr),
    OFFSET(RvStr, ptr),
    OFFSET(RvStr, len),
    SIZE(RvPluginInfo),
    OFFSET(RvPluginInfo, size),
    OFFSET(RvPluginInfo, reserved),
    OFFSET(RvPluginInfo, name),
    OFFSET(RvPluginInfo, version),
    SIZE(RvHost),
    OFFSET(RvHost, size),
    OFFSET(RvHost, api_major),
    OFFSET(RvHost, api_minor),
    OFFSET(RvHost, reserved),
    SIZE(RvLoadArgs),
    OFFSET(RvLoadArgs, size),
    OFFSET(RvLoadArgs, reserved),
    OFFSET(RvLoadArgs, ctx),
    OFFSET(RvLoadArgs, refuse),
    OFFSET(RvLoadArgs, describe_plugin),
    SIZE(RvCall),
    OFFSET(RvCall, size),
    OFFSET(RvCall, verb),
    OFFSET(RvCall, host),
    OFFSET(RvCall, plugin),
    OFFSET(RvCall, pack_run),
    OFFSET(RvCall, args),
    VALUE(RV_OK),
    VALUE(RV_ERR_MISUSE),
    VALUE(RV_ERR_REFUSED),
    VALUE(RV_ERR_UNSUPPORTED),
    VALUE(RV_PLUGIN_LOAD),
    VALUE(RV_PLUGIN_UNLOAD),
    VALUE(RV_API_MAJOR),
    VALUE(RV_API_MINOR),
};

/* The table above, and its length through `count`. */
const RvLayoutEntry *rv_layout(size_t *count) {
    *count = sizeof ENTRIES / sizeof ENTRIES[0];
    return ENTRIES;
}

/* A C Plugin's entry point, defined against the header's declaration, so
   the compiler checks the two agree. It knows no verb. */
RvStatus runvil_plugin(RvCall *call) {
    (void)call;
    return RV_ERR_UNSUPPORTED;
}
