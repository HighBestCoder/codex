/* claw_finstrument.c
 *
 * GCC/Clang -finstrument-functions runtime emitter for Claw's dynamic
 * call-graph collection (M4 + C-4).
 *
 * Build:
 *   clang -shared -fPIC -O2 -DCLAW_FINSTRUMENT_VERSION=\"0.1.0\" \
 *     -fno-instrument-functions \
 *     claw_finstrument.c -ldl -o libclaw_finstrument.so
 *
 * Use:
 *   - Compile target with -finstrument-functions (and -g for symbol info).
 *   - Run binary with CLAW_TRACE_OUTPUT=/path/to/events.jsonl.
 *   - Each function entry writes one JSON line (kind:enter / exit) with
 *     thread id, span id, address, ts_ns, plus dladdr-resolved name.
 *
 * Output format matches claw_perception::dynamic_reconstruct::DynEvent.
 */

#define _GNU_SOURCE

#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <time.h>
#include <stdint.h>
#include <unistd.h>

#ifndef CLAW_FINSTRUMENT_VERSION
#define CLAW_FINSTRUMENT_VERSION "0.1.0"
#endif

static FILE *output = NULL;
static pthread_mutex_t output_lock = PTHREAD_MUTEX_INITIALIZER;
static volatile uint64_t next_span_id = 1;
static __thread uint64_t enter_stack[256];
static __thread int enter_depth = 0;

static uint64_t now_ns(void) __attribute__((no_instrument_function));
static uint64_t now_ns(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static const char *resolve_name(void *fn, char *fallback, size_t cap) __attribute__((no_instrument_function));
static const char *resolve_name(void *fn, char *fallback, size_t cap) {
    Dl_info info;
    if (dladdr(fn, &info) && info.dli_sname) {
        return info.dli_sname;
    }
    snprintf(fallback, cap, "0x%lx", (unsigned long)fn);
    return fallback;
}

static FILE *get_output(void) __attribute__((no_instrument_function));
static FILE *get_output(void) {
    if (output) {
        return output;
    }
    const char *path = getenv("CLAW_TRACE_OUTPUT");
    if (!path || !path[0]) {
        return NULL;
    }
    pthread_mutex_lock(&output_lock);
    if (!output) {
        output = fopen(path, "a");
    }
    pthread_mutex_unlock(&output_lock);
    return output;
}

static uint64_t alloc_span_id(void) __attribute__((no_instrument_function));
static uint64_t alloc_span_id(void) {
    return __atomic_fetch_add(&next_span_id, 1, __ATOMIC_RELAXED);
}

void __cyg_profile_func_enter(void *this_fn, void *call_site)
    __attribute__((no_instrument_function));

void __cyg_profile_func_enter(void *this_fn, void *call_site) {
    (void)call_site;
    FILE *out = get_output();
    if (!out) return;
    char fb[64];
    const char *name = resolve_name(this_fn, fb, sizeof(fb));
    uint64_t span_id = alloc_span_id();
    uint64_t parent = (enter_depth > 0) ? enter_stack[enter_depth - 1] : 0;
    if (enter_depth < (int)(sizeof(enter_stack) / sizeof(enter_stack[0]))) {
        enter_stack[enter_depth++] = span_id;
    }
    pthread_mutex_lock(&output_lock);
    if (parent != 0) {
        fprintf(out,
                "{\"kind\":\"enter\",\"span_id\":%lu,\"parent_span_id\":%lu,"
                "\"fn_name\":\"%s\",\"target\":\"native\",\"ts_ns\":%lu,"
                "\"thread\":\"%lx\"}\n",
                (unsigned long)span_id,
                (unsigned long)parent,
                name,
                (unsigned long)now_ns(),
                (unsigned long)pthread_self());
    } else {
        fprintf(out,
                "{\"kind\":\"enter\",\"span_id\":%lu,\"fn_name\":\"%s\","
                "\"target\":\"native\",\"ts_ns\":%lu,\"thread\":\"%lx\"}\n",
                (unsigned long)span_id,
                name,
                (unsigned long)now_ns(),
                (unsigned long)pthread_self());
    }
    pthread_mutex_unlock(&output_lock);
}

void __cyg_profile_func_exit(void *this_fn, void *call_site)
    __attribute__((no_instrument_function));

void __cyg_profile_func_exit(void *this_fn, void *call_site) {
    (void)this_fn;
    (void)call_site;
    FILE *out = get_output();
    if (!out) return;
    uint64_t span_id = (enter_depth > 0) ? enter_stack[--enter_depth] : 0;
    pthread_mutex_lock(&output_lock);
    fprintf(out,
            "{\"kind\":\"exit\",\"span_id\":%lu,\"ts_ns\":%lu}\n",
            (unsigned long)span_id,
            (unsigned long)now_ns());
    pthread_mutex_unlock(&output_lock);
}

__attribute__((destructor, no_instrument_function))
static void claw_finstrument_shutdown(void) {
    pthread_mutex_lock(&output_lock);
    if (output) {
        fflush(output);
        fclose(output);
        output = NULL;
    }
    pthread_mutex_unlock(&output_lock);
}

const char *claw_finstrument_version(void)
    __attribute__((no_instrument_function));

const char *claw_finstrument_version(void) {
    return CLAW_FINSTRUMENT_VERSION;
}
