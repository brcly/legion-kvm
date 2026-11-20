#ifndef LEGION_STAGE_PROFILE_H
#define LEGION_STAGE_PROFILE_H
#include <stdint.h>

/* CPU CLOCK_MONOTONIC observations, never GPU timestamps. Normal rendering
 * allocates no records and makes no clock calls for this diagnostic. */
struct stage_record {
    uint64_t frame, begin, state, draw, tag, fence, flushed, wait_begin, wait_end;
    uint64_t post_wait, swap_begin, swap_end, gpu_elapsed;
    unsigned waits, timeouts, first_status, final_status;
};
int stage_profile_open(const char *path);
int stage_profile_enabled(void);
struct stage_record *stage_profile_next(uint64_t frame);
int stage_profile_finish(void);
void stage_profile_close(void);
#endif
