/*
 * The entire C++ surface of the Airwindows backend: a flat `extern "C"` API over
 * airwin2rack's consolidated plugin set. Everything above this file is Rust.
 *
 * Two things are deliberately *not* used from airwin2rack:
 *
 *   - `AirwinRegistry.cpp`, because it pulls in CMakeRC (`cmrc`) purely to serve
 *     per-plugin documentation text out of an embedded resource filesystem. We take the
 *     one-line `whatText` that `ModuleAdd.h` already carries instead, so the only thing
 *     that file provided that we want -- the eight static member definitions -- is
 *     restated here and the cmrc dependency disappears entirely.
 *   - `completeRegistry()`, which builds name->index and by-category lookup tables. The
 *     Rust side keys everything by the registry's own index and derives groups from
 *     `aw_category`, so those tables would be a second copy of state we already hold.
 *
 * `ModuleAdd.h` is included exactly once, here. Its ~517 `registerAirwindow({...})` calls
 * are global initializers, so they run before `main` and append to `AirwinRegistry::registry`
 * in the order the file lists them -- which is why the statics below must be *defined above*
 * the include. Within one translation unit initialization order is top-to-bottom and
 * guaranteed; across translation units it is not, which is the other reason this all lives
 * in a single file.
 */

#include "AirwinRegistry.h"

#include <memory>
#include <string>
#include <cstring>

// The static members `AirwinRegistry.cpp` would otherwise define. Only `registry` is ever
// read on this side; the rest exist because `AirwinRegistry.h` declares them and the link
// would fail without them.
std::vector<AirwinRegistry::awReg> AirwinRegistry::registry;
std::set<std::string> AirwinRegistry::categories;
std::vector<int> AirwinRegistry::fxAlphaOrdering;
std::vector<int> AirwinRegistry::fxChrisOrdering;
std::map<std::string, std::vector<std::string>> AirwinRegistry::fxByCategory;
std::map<std::string, std::vector<std::string>> AirwinRegistry::fxByCategoryChrisOrder;
std::unordered_map<std::string, int> AirwinRegistry::nameToIndex;
std::map<std::string, std::unordered_set<std::string>> AirwinRegistry::namesByCollection;

#include "ModuleAdd.h"

namespace
{
bool in_range(int32_t i) { return i >= 0 && (size_t)i < AirwinRegistry::registry.size(); }

// Every plugin's `getParameter*` writes through `vst_strncpy` with `kVstMaxParamStrLen`,
// which does not guarantee a terminator. Give it its own oversized buffer, terminate that,
// then copy out -- so a plugin cannot write past whatever the caller supplied.
void copy_out(void (AirwinConsolidatedBase::*fn)(VstInt32, char *), void *handle, int32_t idx,
              char *buf, int32_t len)
{
    if (!handle || !buf || len <= 0)
        return;
    buf[0] = '\0';
    char scratch[kVstMaxParamStrLen * 4];
    std::memset(scratch, 0, sizeof(scratch));
    auto *p = static_cast<AirwinConsolidatedBase *>(handle);
    (p->*fn)(idx, scratch);
    scratch[sizeof(scratch) - 1] = '\0';
    std::strncpy(buf, scratch, (size_t)len - 1);
    buf[len - 1] = '\0';
}
} // namespace

extern "C"
{
    int32_t aw_count(void) { return (int32_t)AirwinRegistry::registry.size(); }

    // The returned pointers borrow the registry's own `std::string` storage, which lives for
    // the life of the process and is never mutated after static init. Safe to hold as a
    // `&'static CStr` on the Rust side.
    const char *aw_name(int32_t i)
    {
        return in_range(i) ? AirwinRegistry::registry[i].name.c_str() : nullptr;
    }
    const char *aw_category(int32_t i)
    {
        return in_range(i) ? AirwinRegistry::registry[i].category.c_str() : nullptr;
    }
    const char *aw_description(int32_t i)
    {
        return in_range(i) ? AirwinRegistry::registry[i].whatText.c_str() : nullptr;
    }
    int32_t aw_nparams(int32_t i) { return in_range(i) ? AirwinRegistry::registry[i].nParams : 0; }
    int32_t aw_is_mono(int32_t i) { return in_range(i) && AirwinRegistry::registry[i].isMono; }

    /*
     * The sample rate is set on the *base class* before the generator runs, not on the
     * instance afterwards. `AirwinConsolidatedBase` initializes its `sampleRate` member from
     * the static `defaultSampleRate`, and a plugin constructor is free to call
     * `getSampleRate()` while computing its initial filter state -- which asserts the rate is
     * above 2000. Constructing first and setting the rate second would hand those plugins a
     * rate of 0.
     */
    void *aw_create(int32_t i, float sample_rate)
    {
        if (!in_range(i) || !(sample_rate > 2000.f))
            return nullptr;
        AirwinConsolidatedBase::defaultSampleRate = sample_rate;
        auto up = AirwinRegistry::registry[i].generator();
        if (!up)
            return nullptr;
        up->setSampleRate(sample_rate);
        return up.release();
    }

    void aw_destroy(void *h) { delete static_cast<AirwinConsolidatedBase *>(h); }

    void aw_param_name(void *h, int32_t idx, char *buf, int32_t len)
    {
        copy_out(&AirwinConsolidatedBase::getParameterName, h, idx, buf, len);
    }
    void aw_param_display(void *h, int32_t idx, char *buf, int32_t len)
    {
        copy_out(&AirwinConsolidatedBase::getParameterDisplay, h, idx, buf, len);
    }
    void aw_param_label(void *h, int32_t idx, char *buf, int32_t len)
    {
        copy_out(&AirwinConsolidatedBase::getParameterLabel, h, idx, buf, len);
    }

    void aw_set_param(void *h, int32_t idx, float v)
    {
        if (h)
            static_cast<AirwinConsolidatedBase *>(h)->setParameter(idx, v);
    }
    float aw_get_param(void *h, int32_t idx)
    {
        return h ? static_cast<AirwinConsolidatedBase *>(h)->getParameter(idx) : 0.f;
    }

    /*
     * Every Airwindows plugin is hard-wired to two in and two out -- `processReplacing`
     * indexes `inputs[0]`, `inputs[1]`, `outputs[0]`, `outputs[1]` literally, with no loop
     * over a channel count. So the interface is fixed at two legs and the Rust side is
     * responsible for deciding what feeds them (a mono document duplicates into both).
     *
     * Input and output buffers must not alias: plugins read `*in++` and write `*out++`
     * within the same loop iteration, but several also read input *ahead* of the write
     * position across iterations. The caller keeps them separate.
     */
    void aw_process(void *h, float *in_l, float *in_r, float *out_l, float *out_r, int32_t frames)
    {
        if (!h || frames <= 0)
            return;
        float *inputs[2] = {in_l, in_r};
        float *outputs[2] = {out_l, out_r};
        static_cast<AirwinConsolidatedBase *>(h)->processReplacing(inputs, outputs, frames);
    }
}
