#ifndef LEGION_PRESENTATION_H
#define LEGION_PRESENTATION_H
#include <SDL.h>
#include <stdint.h>
#include "frame_tags.h"
struct presentation_stats {
    uint64_t submitted, presented, discarded, abandoned, errors, clock_id;
};
int presentation_active(void);
int presentation_open(SDL_Window *window, const char *path, unsigned delay_ms);
void presentation_begin(uint64_t frame, uint64_t draw_ns, uint64_t gpu_ns,
                        uint64_t device, uint64_t inode, uint32_t width, uint32_t height, const struct frame_tag *tag, uint64_t gpu_elapsed, int early);
void presentation_end(void);
void presentation_read_complete(uint64_t gpu_ns, const struct frame_tag *tag, uint64_t gpu_elapsed);
void presentation_poll(void);
void presentation_unmap(void);
int presentation_finish(struct presentation_stats *out);
void presentation_close(void);
uint64_t presentation_now(void);
#endif
