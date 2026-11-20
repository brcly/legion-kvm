#ifndef LEGION_FRAME_TAGS_H
#define LEGION_FRAME_TAGS_H
#include <stdint.h>
struct frame_tag { uint32_t generation, source_frame, valid; };
int frame_tags_open(void);
int frame_tags_record(unsigned texture);
int frame_tags_read(struct frame_tag *tag);
void frame_tags_close(void);
#endif
