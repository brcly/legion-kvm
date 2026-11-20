#include "stage_profile.h"
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>

#define STAGE_LIMIT 32768u
static struct stage_record *records;
static unsigned used;
static FILE *report;

int stage_profile_open(const char *path)
{
    if (records || report) return -1;
    records=calloc(STAGE_LIMIT,sizeof(*records));
    if (!records) return -1;
    report=fopen(path,"wx");
    if (!report) {free(records);records=NULL;return -1;}
    return 0;
}

int stage_profile_enabled(void) {return records!=NULL;}

struct stage_record *stage_profile_next(uint64_t frame)
{
    if (!records || used==STAGE_LIMIT) return NULL;
    struct stage_record *record=&records[used++];
    record->frame=frame;
    return record;
}

int stage_profile_finish(void)
{
    if (!report) return -1;
    for (unsigned i=0;i<used;i++) {
        const struct stage_record *r=&records[i];
        fprintf(report,"{\"event\":\"stage\",\"frame\":%"PRIu64
            ",\"begin_ns\":%"PRIu64",\"state_ns\":%"PRIu64
            ",\"draw_ns\":%"PRIu64",\"tag_ns\":%"PRIu64
            ",\"fence_ns\":%"PRIu64",\"flushed_ns\":%"PRIu64
            ",\"wait_begin_ns\":%"PRIu64",\"wait_end_ns\":%"PRIu64
            ",\"post_wait_ns\":%"PRIu64",\"swap_begin_ns\":%"PRIu64
            ",\"swap_end_ns\":%"PRIu64
            ",\"gpu_bracket_ns\":%"PRIu64
            ",\"wait_calls\":%u,\"timeouts\":%u,\"first_status\":%u,\"final_status\":%u}\n",
            r->frame,r->begin,r->state,r->draw,r->tag,r->fence,r->flushed,
            r->wait_begin,r->wait_end,r->post_wait,r->swap_begin,r->swap_end,r->gpu_elapsed,
            r->waits,r->timeouts,r->first_status,r->final_status);
    }
    fprintf(report,"{\"event\":\"summary\",\"frames\":%u,\"clock\":\"CLOCK_MONOTONIC\",\"limit\":%u,\"writing\":\"after-rendering\"}\n",used,STAGE_LIMIT);
    int error=ferror(report);
    if (fclose(report)) error=1;
    report=NULL;
    return error ? -1 : 0;
}

void stage_profile_close(void)
{
    if (report) fclose(report);
    report=NULL;
    free(records);records=NULL;used=0;
}
